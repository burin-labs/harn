use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::cli::{ModelsLoraArgs, ModelsLoraCommand, ModelsLoraInspectArgs, ModelsLoraPlanArgs};
use crate::commands::local::runtime::normalize_local_provider_id;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

mod export;
mod manifest;
mod preflight;
mod train;

const LORA_INSPECT_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_INSPECT_PAYLOAD_JSON";
const LORA_INSPECT_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_INSPECT_PAYLOAD_PRETTY";
const LORA_PLAN_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_PLAN_PAYLOAD_JSON";
const LORA_PLAN_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_PLAN_PAYLOAD_PRETTY";
const LORA_CONTRACT_SCHEMA_VERSION: u64 = 2;
const LORA_CONTRACT_HASH_SCHEMA_VERSION: u64 = 2;
const LORA_TRAINING_CONTRACT_SCHEMA_VERSION: u64 = 2;
const LORA_PEFT_SAVE_POLICY_SCHEMA_VERSION: u64 = 1;
const LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION: u64 = 2;
/// Serialises the dispatch path so concurrent in-process callers do not race on
/// the env vars that carry the Rust-collected adapter/catalog facts.
static LORA_RENDER_DISPATCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn run(args: ModelsLoraArgs) {
    let exit_code = match args.command {
        ModelsLoraCommand::Export(args) => Box::pin(export::export_dataset(&args)).await,
        ModelsLoraCommand::Inspect(args) => Box::pin(inspect(&args)).await,
        ModelsLoraCommand::Manifest(args) => Box::pin(manifest::manifest(&args)).await,
        ModelsLoraCommand::Plan(args) => Box::pin(plan(&args)).await,
        ModelsLoraCommand::Preflight(args) => Box::pin(preflight::preflight(&args)).await,
        ModelsLoraCommand::Train(args) => Box::pin(train::train(&args)).await,
    };
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

async fn inspect(args: &ModelsLoraInspectArgs) -> i32 {
    let report = match inspect_report(args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    render_embedded_lora_report(
        &report,
        LORA_INSPECT_PAYLOAD_ENV,
        LORA_INSPECT_PAYLOAD_PRETTY_ENV,
        "models/lora_inspect",
        args.json,
        "LoRA inspect",
    )
    .await
}

async fn plan(args: &ModelsLoraPlanArgs) -> i32 {
    let report = match plan_report(args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    render_embedded_lora_report(
        &report,
        LORA_PLAN_PAYLOAD_ENV,
        LORA_PLAN_PAYLOAD_PRETTY_ENV,
        "models/lora_plan",
        args.json,
        "LoRA plan",
    )
    .await
}

pub(super) async fn render_embedded_lora_report<T: Serialize>(
    report: &T,
    payload_env: &'static str,
    pretty_env: &'static str,
    script_name: &'static str,
    json: bool,
    label: &str,
) -> i32 {
    let payload_json = match serde_json::to_string(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise {label} payload: {error}");
            return 1;
        }
    };
    let pretty_json = match serde_json::to_string_pretty(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to render {label} JSON: {error}");
            return 1;
        }
    };

    let _guard = LORA_RENDER_DISPATCH_LOCK.lock().await;
    let _payload = ScopedEnvVar::set(payload_env, &payload_json);
    let _pretty = ScopedEnvVar::set(pretty_env, &pretty_json);
    let outcome = dispatch::run_embedded_script(script_name, Vec::new(), json).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}

fn inspect_report(args: &ModelsLoraInspectArgs) -> Result<LoraInspectReport, String> {
    if args.require_contract_id && args.manifest.is_none() {
        return Err("--require-contract-id requires --manifest".to_string());
    }
    let resolved = harn_vm::llm_config::resolve_model_info(&args.base_model);
    let provider = args
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(normalize_local_provider_id)
        .unwrap_or_else(|| resolved.provider.clone());
    let provider = normalize_local_provider_id(&provider);
    let catalog = harn_vm::llm_config::model_catalog_entry(&resolved.id);
    let capabilities = harn_vm::llm::capabilities::lookup(&provider, &resolved.id);
    let tool_format = harn_vm::llm_config::default_tool_format(&resolved.id, &provider);
    let adapter = inspect_adapter(&args.adapter, args.name.as_deref())?;
    let local_runtime =
        harn_vm::llm_config::provider_config(&provider).and_then(|provider| provider.local_runtime);
    let provider_lora_module_value_format = lora_modules_value_format(local_runtime.as_ref());
    let provider_supports_lora_launch = local_runtime
        .as_ref()
        .and_then(|runtime| runtime.lora_modules_arg.as_ref())
        .is_some();
    let provider_supports_lora_max_rank = local_runtime
        .as_ref()
        .and_then(|runtime| runtime.max_lora_rank_arg.as_ref())
        .is_some();
    let base_model_match =
        base_model_match(adapter.base_model_name_or_path.as_deref(), &resolved.id);
    let mut warnings = Vec::new();
    if adapter.exists && !adapter.config_found {
        warnings.push("local adapter exists but adapter_config.json was not found".to_string());
    }
    if adapter.exists && adapter.weights_found.is_empty() {
        warnings.push("local adapter has no adapter_model.* weight file".to_string());
    }
    if adapter
        .peft_type
        .as_deref()
        .is_some_and(|peft| peft != "LORA")
    {
        warnings.push("adapter_config.json peft_type is not LORA".to_string());
    }
    if matches!(base_model_match, BaseModelMatch::Mismatch) {
        warnings.push(format!(
            "adapter base_model_name_or_path does not match resolved base model {}",
            resolved.id
        ));
    }
    if !adapter.exists {
        warnings.push(
            "adapter path does not exist locally; treating it as a remote/runtime-resolved id"
                .to_string(),
        );
    }
    if !provider_supports_lora_launch {
        warnings.push(format!(
            "provider {provider} does not declare local-runtime LoRA launch flags"
        ));
    }
    if adapter.rank.is_some() && provider_supports_lora_launch && !provider_supports_lora_max_rank {
        warnings.push(format!(
            "adapter rank is known but provider {provider} does not declare a max LoRA rank flag"
        ));
    }
    let contract = inspect_contract_report(
        args.manifest.as_deref(),
        args.require_contract_id,
        &adapter,
        &resolved.id,
        &provider,
        &tool_format,
    )?;
    if let Some(contract) = &contract {
        warnings.extend(contract.warnings.clone());
    }
    let ok = warnings.iter().all(|warning| {
        !warning.starts_with("local adapter exists")
            && !warning.starts_with("adapter_config.json peft_type")
            && !warning.starts_with("adapter base_model_name_or_path")
            && !warning.starts_with("LoRA contract mismatch")
            && !warning.starts_with("LoRA contract missing")
    });
    let request_model = adapter.name.clone();
    let max_lora_rank = adapter
        .rank
        .filter(|_| provider_supports_lora_launch && provider_supports_lora_max_rank);
    let harn_local_launch = if provider_supports_lora_launch {
        let model_source = adapter
            .base_model_name_or_path
            .clone()
            .unwrap_or_else(|| resolved.id.clone());
        let mut command = vec![
            "harn".to_string(),
            "local".to_string(),
            "launch".to_string(),
            args.base_model.clone(),
            "--provider".to_string(),
            provider.clone(),
            "--model-source".to_string(),
            model_source,
            "--lora-adapter".to_string(),
            format!("{}={}", adapter.name, adapter.input),
        ];
        if let Some(rank) = max_lora_rank {
            command.extend(["--max-lora-rank".to_string(), rank.to_string()]);
        }
        command
    } else {
        Vec::new()
    };
    let serving = InspectServingReport {
        request_model: request_model.clone(),
        base_model: resolved.id.clone(),
        provider: provider.clone(),
        tool_format: tool_format.clone(),
        lora_module_value_format: provider_lora_module_value_format.clone(),
        serving_requirements: tool_call_serving_requirements(&resolved.id, &provider, &tool_format),
    };
    Ok(LoraInspectReport {
        ok,
        base: BaseModelReport {
            selector: args.base_model.clone(),
            id: resolved.id.clone(),
            provider,
            resolved_alias: resolved.alias,
            tool_format,
            tier: resolved.tier,
            family: resolved.family,
            lineage: resolved.lineage,
            catalog_name: catalog.as_ref().map(|model| model.name.clone()),
            context_window: catalog.as_ref().map(|model| model.context_window),
        },
        adapter,
        contract,
        compatibility: CompatibilityReport {
            base_model_match,
            provider_supports_lora_launch,
            provider_supports_lora_max_rank,
            provider_lora_module_value_format,
        },
        tool_calling: ToolCallingReport {
            native_tools: capabilities.native_tools,
            preferred_tool_format: capabilities.preferred_tool_format,
            text_tool_wire_format_supported: capabilities.text_tool_wire_format_supported,
            structured_output_mode: capabilities.structured_output_mode,
            recommended_endpoint: capabilities.recommended_endpoint,
        },
        serving,
        launch: LaunchHints {
            request_model,
            max_lora_rank,
            harn_local_launch,
        },
        warnings,
    })
}

fn inspect_contract_report(
    manifest_path: Option<&Path>,
    require_adapter_contract_id: bool,
    adapter: &AdapterReport,
    resolved_base_model: &str,
    provider: &str,
    tool_format: &str,
) -> Result<Option<InspectContractReport>, String> {
    let Some(path) = manifest_path else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read manifest {}: {error}", path.display()))?;
    let manifest = serde_json::from_str::<serde_json::Value>(&raw)
        .map_err(|error| format!("failed to parse manifest {}: {error}", path.display()))?;
    let contract = manifest
        .get("contract")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("manifest {} is missing contract object", path.display()))?;
    let contract_id = manifest_string_from_object(contract, "id");
    let manifest_base_model = manifest_string_from_object(contract, "base_model");
    let manifest_provider = manifest_string_from_object(contract, "provider");
    let manifest_tool_format = manifest_string_from_object(contract, "harn_tool_format");
    let manifest_dataset_format = manifest_string_from_object(contract, "dataset_format");
    let manifest_chat_template = manifest_string_from_object(contract, "chat_template");
    let manifest_modules_to_save = manifest_modules_to_save_from_contract(contract);
    let target_adapter_name = manifest
        .get("target")
        .and_then(serde_json::Value::as_object)
        .and_then(|target| manifest_string_from_object(target, "adapter_name"));
    let serving_request_model = manifest
        .get("serving")
        .and_then(serde_json::Value::as_object)
        .and_then(|serving| manifest_string_from_object(serving, "request_model"));

    let base_model_match = base_model_match(manifest_base_model.as_deref(), resolved_base_model);
    let provider_matches = manifest_provider
        .as_deref()
        .is_some_and(|manifest_provider| manifest_provider == provider);
    let tool_format_matches = manifest_tool_format
        .as_deref()
        .is_some_and(|manifest_tool_format| manifest_tool_format == tool_format);
    let adapter_name_expectations = [
        target_adapter_name.as_deref(),
        serving_request_model.as_deref(),
    ];
    let adapter_name_matches = if adapter_name_expectations.iter().any(Option::is_some) {
        Some(
            adapter_name_expectations
                .into_iter()
                .flatten()
                .all(|expected| expected == adapter.name),
        )
    } else {
        None
    };
    let adapter_contract_id_matches = match (&adapter.contract_id, &contract_id) {
        (Some(adapter_id), Some(manifest_id)) => Some(adapter_id == manifest_id),
        _ => None,
    };
    let adapter_modules_to_save = normalize_modules_to_save_lossy(adapter.modules_to_save.clone());
    let modules_to_save_matches = manifest_modules_to_save
        .as_ref()
        .map(|manifest_modules| manifest_modules == &adapter_modules_to_save);

    let mut warnings = Vec::new();
    if matches!(
        base_model_match,
        BaseModelMatch::Mismatch | BaseModelMatch::Unknown
    ) {
        warnings.push(format!(
            "LoRA contract mismatch: manifest base_model={} does not match resolved base {}",
            manifest_base_model.as_deref().unwrap_or("<missing>"),
            resolved_base_model
        ));
    }
    if !provider_matches {
        warnings.push(format!(
            "LoRA contract mismatch: manifest provider={} does not match provider {}",
            manifest_provider.as_deref().unwrap_or("<missing>"),
            provider
        ));
    }
    if !tool_format_matches {
        warnings.push(format!(
            "LoRA contract mismatch: manifest tool format={} does not match route tool format {}",
            manifest_tool_format.as_deref().unwrap_or("<missing>"),
            tool_format
        ));
    }
    if contract_id.is_none() {
        warnings.push("LoRA contract mismatch: manifest contract.id is missing".to_string());
    }
    if adapter_name_matches == Some(false) {
        warnings.push(format!(
            "LoRA contract mismatch: manifest adapter/request model does not match adapter name {}",
            adapter.name
        ));
    }
    if adapter_contract_id_matches == Some(false) {
        warnings.push(format!(
            "LoRA contract mismatch: adapter contract id {} does not match manifest contract id {}",
            adapter.contract_id.as_deref().unwrap_or("<missing>"),
            contract_id.as_deref().unwrap_or("<missing>")
        ));
    }
    if manifest_modules_to_save.is_none() {
        warnings.push("LoRA contract mismatch: manifest PEFT save policy is missing".to_string());
    } else if modules_to_save_matches == Some(false) {
        warnings.push(format!(
            "LoRA contract mismatch: adapter modules_to_save {:?} does not match manifest modules_to_save {:?}",
            adapter_modules_to_save,
            manifest_modules_to_save.as_deref().unwrap_or(&[])
        ));
    }
    if adapter.contract_id.is_none() {
        let prefix = if require_adapter_contract_id {
            "LoRA contract missing"
        } else {
            "LoRA contract warning"
        };
        warnings.push(format!(
            "{prefix}: adapter_config.json does not include harn_lora_contract_id"
        ));
    }

    let status = if warnings.iter().any(|warning| {
        warning.starts_with("LoRA contract mismatch")
            || warning.starts_with("LoRA contract missing")
    }) {
        ContractCheckStatus::Fail
    } else if warnings.is_empty() {
        ContractCheckStatus::Pass
    } else {
        ContractCheckStatus::Warn
    };

    Ok(Some(InspectContractReport {
        manifest_path: path.display().to_string(),
        contract_id,
        adapter_contract_id: adapter.contract_id.clone(),
        status,
        base_model_match,
        provider_matches,
        tool_format_matches,
        adapter_name_matches,
        modules_to_save_matches,
        require_adapter_contract_id,
        manifest: InspectContractManifest {
            base_model: manifest_base_model,
            provider: manifest_provider,
            harn_tool_format: manifest_tool_format,
            dataset_format: manifest_dataset_format,
            chat_template: manifest_chat_template,
            modules_to_save: manifest_modules_to_save,
            adapter_name: target_adapter_name,
            request_model: serving_request_model,
        },
        warnings,
    }))
}

fn manifest_modules_to_save_from_contract(
    contract: &serde_json::Map<String, serde_json::Value>,
) -> Option<Vec<String>> {
    let modules = contract
        .get("training_contract")?
        .get("peft_save_policy")?
        .get("modules_to_save")?;
    Some(normalize_modules_to_save_lossy(value_string_list(modules)))
}

fn manifest_string_from_object(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<String> {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn plan_report(args: &ModelsLoraPlanArgs) -> Result<LoraPlanReport, String> {
    let method = normalize_lora_method(&args.method)?;
    let trainer = normalize_lora_trainer(&args.trainer)?;
    let rank = normalize_lora_rank(args.rank)?;
    let alpha = normalize_lora_alpha(args.alpha, rank)?;
    let dropout = normalize_lora_dropout(args.dropout)?;
    let quantization = quantization_for_method(&method).to_string();
    let precision = precision_contract_for_method(&method);
    let requested_tool_format = normalize_plan_tool_format(&args.tool_format)?;
    let requested_corpus_strategy = normalize_corpus_strategy(&args.corpus_strategy)?;
    let resolved = harn_vm::llm_config::resolve_model_info(&args.base_model);
    let modules_to_save = normalize_modules_to_save(&args.modules_to_save)?;
    let target_modules =
        target_modules_for_route(&method, &resolved.id, &resolved.family, &resolved.lineage);
    let provider = args
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(normalize_local_provider_id)
        .unwrap_or_else(|| resolved.provider.clone());
    let provider = normalize_local_provider_id(&provider);
    let catalog = harn_vm::llm_config::model_catalog_entry(&resolved.id);
    let capabilities = harn_vm::llm::capabilities::lookup(&provider, &resolved.id);
    let catalog_default_tool_format =
        harn_vm::llm_config::default_tool_format(&resolved.id, &provider);
    let decision = if requested_tool_format == "auto" {
        harn_vm::llm::capabilities::ToolFormatDecision {
            effective: catalog_default_tool_format.clone(),
            correction: None,
        }
    } else {
        harn_vm::llm::capabilities::validate_tool_format(
            &provider,
            &resolved.id,
            &requested_tool_format,
        )
    };
    let dataset_format = dataset_format_for_tool_format(&decision.effective);
    let request_model = "ADAPTER_MODEL".to_string();
    let adapter_name = "ADAPTER_NAME".to_string();
    let adapter_ref = "ADAPTER_PATH_OR_REPO".to_string();
    let corpus = args
        .corpus
        .as_ref()
        .map(|corpus| corpus.trim().to_string())
        .filter(|corpus| !corpus.is_empty());
    let teacher = args
        .teacher
        .as_ref()
        .map(|selector| teacher_report(selector));
    let effective_corpus_strategy = effective_corpus_strategy(
        &requested_corpus_strategy,
        corpus.as_deref(),
        teacher.as_ref(),
    );
    let dataset_arg = corpus
        .clone()
        .unwrap_or_else(|| "conformance/tool-call-eval".to_string());
    let template = template_recipe_for_route(
        &resolved.id,
        &resolved.family,
        &resolved.lineage,
        &decision.effective,
    );
    let contract_id = lora_contract_id(
        &resolved.id,
        &provider,
        &decision.effective,
        dataset_format,
        Some(&template.name),
        &modules_to_save,
    )?;
    let inspect_command = vec![
        "harn".to_string(),
        "models".to_string(),
        "lora".to_string(),
        "inspect".to_string(),
        "--base".to_string(),
        args.base_model.clone(),
        "--provider".to_string(),
        provider.clone(),
        "--name".to_string(),
        adapter_name.clone(),
        adapter_ref.clone(),
    ];
    let local_runtime =
        harn_vm::llm_config::provider_config(&provider).and_then(|provider| provider.local_runtime);
    let lora_module_value_format = lora_modules_value_format(local_runtime.as_ref());
    let provider_supports_lora_launch = local_runtime
        .as_ref()
        .and_then(|runtime| runtime.lora_modules_arg.as_ref())
        .is_some();
    let serving = serving_recipe(
        &resolved.id,
        &provider,
        &request_model,
        &adapter_name,
        &decision.effective,
        dataset_format,
        provider_supports_lora_launch,
        &lora_module_value_format,
    );
    let launch_command = if provider_supports_lora_launch {
        let mut command = vec![
            "harn".to_string(),
            "local".to_string(),
            "launch".to_string(),
            args.base_model.clone(),
            "--provider".to_string(),
            provider.clone(),
            "--model-source".to_string(),
            resolved.id.clone(),
            "--lora-adapter".to_string(),
            format!("{adapter_name}={adapter_ref}"),
        ];
        if local_runtime
            .as_ref()
            .and_then(|runtime| runtime.max_lora_rank_arg.as_ref())
            .is_some()
        {
            command.extend(["--max-lora-rank".to_string(), rank.to_string()]);
        }
        command
    } else {
        Vec::new()
    };
    let eval_dataset = dataset_arg.clone();
    let eval_command = vec![
        "harn".to_string(),
        "eval".to_string(),
        "tool-calls".to_string(),
        "--planner".to_string(),
        request_model.clone(),
        "--tool-format".to_string(),
        decision.effective.clone(),
        "--dataset".to_string(),
        dataset_arg,
    ];
    let export_corpus_arg = corpus
        .clone()
        .unwrap_or_else(|| "CORPUS_JSONL_OR_DIR".to_string());
    let preflight_command = vec![
        "harn".to_string(),
        "models".to_string(),
        "lora".to_string(),
        "preflight".to_string(),
        "--base".to_string(),
        args.base_model.clone(),
        "--provider".to_string(),
        provider.clone(),
        "--tool-format".to_string(),
        decision.effective.clone(),
        "--corpus".to_string(),
        export_corpus_arg.clone(),
        "--source-tool-format".to_string(),
        source_tool_format_required_for_target(&decision.effective).to_string(),
        "--check".to_string(),
    ];
    let mut export_command = vec![
        "harn".to_string(),
        "models".to_string(),
        "lora".to_string(),
        "export".to_string(),
        "--base".to_string(),
        args.base_model.clone(),
        "--provider".to_string(),
        provider.clone(),
        "--tool-format".to_string(),
        decision.effective.clone(),
        "--corpus".to_string(),
        export_corpus_arg.clone(),
        "--out".to_string(),
        "ADAPTER_DATASET.jsonl".to_string(),
        "--manifest".to_string(),
        "ADAPTER_DATASET.manifest.json".to_string(),
        "--adapter-name".to_string(),
        adapter_name.clone(),
        "--chat-template".to_string(),
        template.name.clone(),
    ];
    export_command.extend(precision_target_metadata(&precision));
    export_command.extend(modules_to_save_args(&modules_to_save));
    let mut train_command = vec![
        "harn".to_string(),
        "models".to_string(),
        "lora".to_string(),
        "train".to_string(),
        "--base".to_string(),
        args.base_model.clone(),
        "--provider".to_string(),
        provider.clone(),
        "--tool-format".to_string(),
        decision.effective.clone(),
        "--dataset".to_string(),
        "ADAPTER_DATASET.jsonl".to_string(),
        "--export-manifest".to_string(),
        "ADAPTER_DATASET.manifest.json".to_string(),
        "--output-dir".to_string(),
        "ADAPTER_OUTPUT_DIR".to_string(),
        "--receipt-out".to_string(),
        "ADAPTER_OUTPUT_DIR/train.receipt.json".to_string(),
        "--adapter-name".to_string(),
        adapter_name.clone(),
        "--request-model".to_string(),
        request_model.clone(),
        "--chat-template".to_string(),
        template.name.clone(),
        "--trainer".to_string(),
        trainer.clone(),
        "--method".to_string(),
        method.clone(),
        "--rank".to_string(),
        rank.to_string(),
        "--alpha".to_string(),
        alpha.to_string(),
        "--dropout".to_string(),
        dropout.to_string(),
    ];
    if let Some(teacher) = &teacher {
        train_command.extend(["--teacher".to_string(), teacher.selector.clone()]);
    }
    train_command.extend(precision_target_metadata(&precision));
    train_command.extend(modules_to_save_args(&modules_to_save));
    train_command.extend(target_metadata_args_from_map(&serving_target_metadata(
        &serving,
    )));
    let mut manifest_command = vec![
        "harn".to_string(),
        "models".to_string(),
        "lora".to_string(),
        "manifest".to_string(),
        "--base".to_string(),
        args.base_model.clone(),
        "--provider".to_string(),
        provider.clone(),
        "--tool-format".to_string(),
        decision.effective.clone(),
        "--dataset".to_string(),
        "ADAPTER_DATASET.jsonl".to_string(),
        "--corpus".to_string(),
        export_corpus_arg,
        "--export-manifest".to_string(),
        "ADAPTER_DATASET.manifest.json".to_string(),
        "--adapter-name".to_string(),
        adapter_name,
        "--adapter-path".to_string(),
        adapter_ref,
        "--request-model".to_string(),
        request_model.clone(),
        "--chat-template".to_string(),
        template.name.clone(),
        "--trainer".to_string(),
        trainer.clone(),
        "--method".to_string(),
        method.clone(),
        "--rank".to_string(),
        rank.to_string(),
        "--alpha".to_string(),
        alpha.to_string(),
        "--dropout".to_string(),
        dropout.to_string(),
    ];
    if let Some(teacher) = &teacher {
        manifest_command.extend(["--teacher".to_string(), teacher.selector.clone()]);
    }
    manifest_command.extend(precision_target_metadata(&precision));
    manifest_command.extend(modules_to_save_args(&modules_to_save));
    manifest_command.extend(target_metadata_args_from_map(&serving_target_metadata(
        &serving,
    )));
    let tool_probe_command = vec![
        "harn".to_string(),
        "provider".to_string(),
        "tool-probe".to_string(),
        provider.clone(),
        "--model".to_string(),
        request_model.clone(),
        "--mode".to_string(),
        "both".to_string(),
        "--repeat".to_string(),
        "5".to_string(),
        "--json".to_string(),
    ];
    let mut warnings = plan_warnings(
        &provider,
        &decision,
        provider_supports_lora_launch,
        capabilities.native_tools,
        &requested_tool_format,
        &requested_corpus_strategy,
        &effective_corpus_strategy,
        teacher.as_ref(),
    );
    if decision.effective == "native"
        && provider == "vllm"
        && is_gemma4_route(&resolved.id, &resolved.family, &resolved.lineage)
    {
        warnings.push(
            "Gemma 4 native tool parsing under vLLM is part of the serving contract; serialize validation/eval traffic or pin a parser version proven concurrency-safe"
                .to_string(),
        );
    }
    Ok(LoraPlanReport {
        ok: true,
        base: BaseModelReport {
            selector: args.base_model.clone(),
            id: resolved.id.clone(),
            provider: provider.clone(),
            resolved_alias: resolved.alias,
            tool_format: catalog_default_tool_format,
            tier: resolved.tier,
            family: resolved.family,
            lineage: resolved.lineage,
            catalog_name: catalog.as_ref().map(|model| model.name.clone()),
            context_window: catalog.as_ref().map(|model| model.context_window),
        },
        request: PlanRequest {
            method,
            requested_tool_format,
            effective_tool_format: decision.effective.clone(),
            tool_format_correction: decision.correction,
            corpus,
            requested_corpus_strategy,
            effective_corpus_strategy: effective_corpus_strategy.clone(),
            teacher: teacher.clone(),
        },
        tool_calling: ToolCallingReport {
            native_tools: capabilities.native_tools,
            preferred_tool_format: capabilities.preferred_tool_format,
            text_tool_wire_format_supported: capabilities.text_tool_wire_format_supported,
            structured_output_mode: capabilities.structured_output_mode,
            recommended_endpoint: capabilities.recommended_endpoint,
        },
        training: TrainingRecipe {
            adapter_type: "peft_lora".to_string(),
            trainer: trainer.clone(),
            rank,
            alpha,
            dropout,
            quantization,
            loss_scope: "assistant_tool_calls".to_string(),
            packing: "off_by_default_for_tool_boundaries".to_string(),
            target_modules,
            contract: lora_training_contract(dataset_format, &decision.effective, &modules_to_save),
            trainer_contract: trainer_contract_for_dataset(
                dataset_format,
                &decision.effective,
                &trainer,
                &modules_to_save,
            ),
            notes: training_notes(&decision.effective),
        },
        precision,
        template,
        data: DataRecipe {
            dataset_format: dataset_format.to_string(),
            required_columns: required_columns_for_dataset(dataset_format),
            validation: validation_steps_for_dataset(dataset_format),
        },
        corpus_refresh: corpus_refresh_recipe(
            &effective_corpus_strategy,
            teacher.as_ref(),
            &decision.effective,
            dataset_format,
        ),
        evaluation: lora_evaluation_recipe(
            &contract_id,
            &resolved.id,
            &provider,
            &request_model,
            &decision.effective,
            &eval_dataset,
            eval_command,
        ),
        serving,
        launch: PlanLaunchHints {
            preflight_command,
            export_command,
            train_command,
            manifest_command,
            inspect_command,
            local_launch_command: launch_command,
            tool_probe_command,
            request_model,
        },
        warnings,
    })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn inspect_adapter(input: &str, explicit_name: Option<&str>) -> Result<AdapterReport, String> {
    let expanded = expand_home(input);
    let path = PathBuf::from(&expanded);
    let exists = path.exists();
    let adapter_dir = if path.is_file()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "adapter_config.json")
    {
        path.parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        path
    };
    let config_path = adapter_dir.join("adapter_config.json");
    let config_found = config_path.is_file();
    let config = if config_found {
        let raw = std::fs::read_to_string(&config_path)
            .map_err(|error| format!("failed to read {}: {error}", config_path.display()))?;
        Some(
            serde_json::from_str::<serde_json::Value>(&raw)
                .map_err(|error| format!("failed to parse {}: {error}", config_path.display()))?,
        )
    } else {
        None
    };
    let weights_found = adapter_weights(&adapter_dir);
    Ok(AdapterReport {
        input: input.to_string(),
        name: explicit_name
            .map(str::to_string)
            .unwrap_or_else(|| adapter_name_from_input(input)),
        local_path: exists.then(|| adapter_dir.display().to_string()),
        exists,
        config_found,
        config_path: config_found.then(|| config_path.display().to_string()),
        weights_found,
        peft_type: config_string(&config, "peft_type"),
        task_type: config_string(&config, "task_type"),
        base_model_name_or_path: config_string(&config, "base_model_name_or_path"),
        rank: config_u64(&config, "r"),
        lora_alpha: config_f64(&config, "lora_alpha"),
        target_modules: config_string_list(&config, "target_modules"),
        modules_to_save: normalize_modules_to_save_lossy(config_string_list(
            &config,
            "modules_to_save",
        )),
        contract_id: config_contract_id(&config),
    })
}

fn adapter_weights(dir: &Path) -> Vec<String> {
    ["adapter_model.safetensors", "adapter_model.bin"]
        .into_iter()
        .filter_map(|name| {
            let path = dir.join(name);
            path.is_file().then(|| path.display().to_string())
        })
        .collect()
}

fn config_string(config: &Option<serde_json::Value>, key: &str) -> Option<String> {
    config.as_ref()?.get(key)?.as_str().map(str::to_string)
}

fn config_u64(config: &Option<serde_json::Value>, key: &str) -> Option<u64> {
    config.as_ref()?.get(key)?.as_u64()
}

fn config_f64(config: &Option<serde_json::Value>, key: &str) -> Option<f64> {
    let value = config.as_ref()?.get(key)?;
    value.as_f64().or_else(|| value.as_u64().map(|n| n as f64))
}

fn config_string_list(config: &Option<serde_json::Value>, key: &str) -> Vec<String> {
    let Some(value) = config.as_ref().and_then(|value| value.get(key)) else {
        return Vec::new();
    };
    value_string_list(value)
}

fn value_string_list(value: &serde_json::Value) -> Vec<String> {
    if let Some(text) = value.as_str() {
        return vec![text.to_string()];
    }
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn config_contract_id(config: &Option<serde_json::Value>) -> Option<String> {
    [
        "harn_lora_contract_id",
        "lora_contract_id",
        "harn_contract_id",
    ]
    .into_iter()
    .find_map(|key| config_string(config, key))
}

fn base_model_match(declared: Option<&str>, resolved_id: &str) -> BaseModelMatch {
    let Some(declared) = declared.map(str::trim).filter(|value| !value.is_empty()) else {
        return BaseModelMatch::Unknown;
    };
    let declared = normalize_model_name(declared);
    let resolved = normalize_model_name(resolved_id);
    if declared == resolved {
        return BaseModelMatch::Exact;
    }
    let declared_tail = declared.rsplit('/').next().unwrap_or(&declared);
    let resolved_tail = resolved.rsplit('/').next().unwrap_or(&resolved);
    if declared_tail == resolved_tail {
        BaseModelMatch::Suffix
    } else {
        BaseModelMatch::Mismatch
    }
}

fn normalize_model_name(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("models/")
        .to_ascii_lowercase()
}

fn adapter_name_from_input(input: &str) -> String {
    input
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or("lora-adapter")
        .to_string()
}

fn expand_home(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest).display().to_string();
        }
    }
    value.to_string()
}

fn normalize_lora_method(raw: &str) -> Result<String, String> {
    let method = raw.trim().to_ascii_lowercase();
    match method.as_str() {
        "lora" | "qlora" => Ok(method),
        _ => Err(format!(
            "unsupported LoRA method `{raw}`; expected `qlora` or `lora`"
        )),
    }
}

fn normalize_lora_trainer(raw: &str) -> Result<String, String> {
    let trainer = raw.trim().to_ascii_lowercase().replace('-', "_");
    match trainer.as_str() {
        "trl" | "trl_sft" | "trl_sft_trainer" => Ok("trl_sft_trainer".to_string()),
        "unsloth" | "unsloth_sft" | "unsloth_trl_sft" => Ok("unsloth_sft".to_string()),
        "external" | "external_sft" | "external_sft_trainer" => {
            Ok("external_sft_trainer".to_string())
        }
        _ => Err(format!(
            "unsupported LoRA trainer `{raw}`; expected `trl_sft_trainer`, `unsloth_sft`, or `external_sft_trainer`"
        )),
    }
}

fn normalize_lora_rank(rank: u32) -> Result<u32, String> {
    if rank == 0 {
        return Err("--rank must be greater than 0".to_string());
    }
    Ok(rank)
}

fn normalize_lora_alpha(alpha: Option<u32>, rank: u32) -> Result<u32, String> {
    let alpha = alpha.unwrap_or_else(|| rank.saturating_mul(2));
    if alpha == 0 {
        return Err("--alpha must be greater than 0".to_string());
    }
    Ok(alpha)
}

fn normalize_lora_dropout(dropout: f64) -> Result<f64, String> {
    if !dropout.is_finite() || !(0.0..1.0).contains(&dropout) {
        return Err("--dropout must be a finite value in [0.0, 1.0)".to_string());
    }
    Ok(dropout)
}

fn target_modules_for_route(
    method: &str,
    model_id: &str,
    family: &str,
    lineage: &str,
) -> Vec<String> {
    match method {
        "qlora" => vec!["all-linear".to_string()],
        _ if is_gemma4_route(model_id, family, lineage) => vec![
            "q_proj".to_string(),
            "k_proj".to_string(),
            "v_proj".to_string(),
            "o_proj".to_string(),
            "gate_proj".to_string(),
            "up_proj".to_string(),
            "down_proj".to_string(),
        ],
        _ => vec![
            "q_proj".to_string(),
            "k_proj".to_string(),
            "v_proj".to_string(),
            "o_proj".to_string(),
        ],
    }
}

pub(super) fn normalize_modules_to_save(raw: &[String]) -> Result<Vec<String>, String> {
    let mut seen = BTreeSet::new();
    for item in raw {
        for piece in item.split(',') {
            let module = piece.trim();
            if module.is_empty() {
                return Err("--modules-to-save entries must not be empty".to_string());
            }
            seen.insert(module.to_string());
        }
    }
    Ok(seen.into_iter().collect())
}

fn normalize_modules_to_save_lossy(raw: Vec<String>) -> Vec<String> {
    normalize_modules_to_save(&raw).unwrap_or(raw)
}

fn modules_to_save_args(modules: &[String]) -> Vec<String> {
    modules
        .iter()
        .flat_map(|module| ["--modules-to-save".to_string(), module.clone()])
        .collect()
}

fn normalize_plan_tool_format(raw: &str) -> Result<String, String> {
    let tool_format = raw.trim().to_ascii_lowercase();
    match tool_format.as_str() {
        "auto" | "native" | "text" | "json" => Ok(tool_format),
        _ => Err(format!(
            "unsupported tool format `{raw}`; expected `auto`, `native`, `text`, or `json`"
        )),
    }
}

fn normalize_corpus_strategy(raw: &str) -> Result<String, String> {
    let strategy = raw.trim().to_ascii_lowercase();
    match strategy.as_str() {
        "auto" | "audit-only" | "refresh" | "distill" => Ok(strategy),
        _ => Err(format!(
            "unsupported corpus strategy `{raw}`; expected `auto`, `audit-only`, `refresh`, or `distill`"
        )),
    }
}

fn parse_target_metadata(raw: &[String]) -> Result<BTreeMap<String, String>, String> {
    let mut metadata = BTreeMap::new();
    for item in raw {
        let Some((key, value)) = item.split_once('=') else {
            return Err(format!(
                "invalid --target-metadata `{item}`; expected KEY=VALUE"
            ));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("invalid --target-metadata `{item}`; key is empty"));
        }
        metadata.insert(key.to_string(), value.to_string());
    }
    Ok(metadata)
}

fn effective_corpus_strategy(
    requested: &str,
    corpus: Option<&str>,
    teacher: Option<&TeacherReport>,
) -> String {
    if requested != "auto" {
        return requested.to_string();
    }
    if teacher.is_none() {
        return "audit-only".to_string();
    }
    if corpus.is_some() {
        "refresh".to_string()
    } else {
        "distill".to_string()
    }
}

fn quantization_for_method(method: &str) -> &'static str {
    match method {
        "qlora" => "4bit_base_model",
        "lora" => "base_model_precision",
        _ => unreachable!("normalize_lora_method returned an unsupported method"),
    }
}

fn precision_contract_for_method(method: &str) -> PrecisionContract {
    let (
        training_base_precision,
        serving_base_precision,
        compatibility_policy,
    ) = match method {
        "qlora" => (
            "4bit_nf4_or_runtime_equivalent",
            "same_quantization_family_as_training_or_revalidate",
            "changing the base quantization or compute dtype makes a new route until promotion gates pass",
        ),
        "lora" => (
            "base_model_precision",
            "same_base_model_precision_as_training_or_revalidate",
            "changing the base or adapter precision makes a new route until promotion gates pass",
        ),
        _ => unreachable!("normalize_lora_method returned an unsupported method"),
    };
    PrecisionContract {
        schema_version: 1,
        training_base_precision: training_base_precision.to_string(),
        training_compute_precision: "bf16_when_supported_else_fp16".to_string(),
        adapter_weight_precision: "bf16_or_fp16_lora_weights".to_string(),
        serving_base_precision: serving_base_precision.to_string(),
        serving_adapter_precision: "load_adapter_weights_without_merge_until_promotion".to_string(),
        compatibility_policy: compatibility_policy.to_string(),
        promotion_gates: vec![
            "record training base precision, compute precision, adapter precision, and serving base precision in the adapter manifest".to_string(),
            "compare base versus adapter using the same base precision planned for serving".to_string(),
            "rerun promotion gates whenever quantization, compute dtype, chat template, or tool format changes".to_string(),
        ],
    }
}

fn precision_target_metadata(precision: &PrecisionContract) -> Vec<String> {
    [
        (
            "training_base_precision",
            precision.training_base_precision.as_str(),
        ),
        (
            "training_compute_precision",
            precision.training_compute_precision.as_str(),
        ),
        (
            "adapter_weight_precision",
            precision.adapter_weight_precision.as_str(),
        ),
        (
            "serving_base_precision",
            precision.serving_base_precision.as_str(),
        ),
    ]
    .into_iter()
    .flat_map(|(key, value)| ["--target-metadata".to_string(), format!("{key}={value}")])
    .collect()
}

fn merge_serving_target_metadata(
    metadata: &mut BTreeMap<String, String>,
    serving: &ServingRecipe,
    warnings: &mut Vec<String>,
) {
    for (key, value) in serving_target_metadata(serving) {
        if let Some(existing) = metadata.get(&key) {
            if existing != &value {
                warnings.push(format!(
                    "--target-metadata {key}={existing} overrides Harn-derived serving metadata {key}={value}; verify the manifest records the actual serving route"
                ));
            }
        } else {
            metadata.insert(key, value);
        }
    }
}

fn target_metadata_args_from_map(metadata: &BTreeMap<String, String>) -> Vec<String> {
    metadata
        .iter()
        .flat_map(|(key, value)| ["--target-metadata".to_string(), format!("{key}={value}")])
        .collect()
}

fn serving_target_metadata(serving: &ServingRecipe) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "serving_adapter_binding".to_string(),
        serving.adapter_binding.clone(),
    );
    metadata.insert(
        "serving_lora_modules_value_format".to_string(),
        serving.lora_module_value_format.clone(),
    );
    metadata.insert("serving_provider".to_string(), serving.provider.clone());
    metadata.insert(
        "serving_request_model".to_string(),
        serving.request_model.clone(),
    );
    for requirement in &serving.serving_requirements {
        match (
            requirement.kind.as_str(),
            requirement.name.as_str(),
            requirement.value.as_deref(),
        ) {
            ("parser_owner", "tool_call_parser", Some(value)) => {
                metadata.insert("serving_tool_parser_owner".to_string(), value.to_string());
            }
            ("provider_native_tool_parser", "native_tool_parser_mode", Some(value)) => {
                metadata.insert("provider_native_tool_parser".to_string(), value.to_string());
            }
            ("server_flag", "--enable-auto-tool-choice", _) => {
                metadata.insert("vllm_auto_tool_choice".to_string(), "required".to_string());
            }
            ("server_flag", "--tool-call-parser", Some(value)) => {
                metadata.insert("tool_parser_id".to_string(), value.to_string());
            }
            ("server_flag", "--reasoning-parser", Some(value)) => {
                metadata.insert("reasoning_parser".to_string(), value.to_string());
            }
            ("chat_template", "chat_template", Some(value)) => {
                metadata.insert("serving_chat_template".to_string(), value.to_string());
            }
            ("stop_sequence", "inference_stop_sequence", Some(value)) => {
                metadata.insert("inference_stop_sequence".to_string(), value.to_string());
            }
            ("manifest_metadata", "tool_parser_id", Some(value)) => {
                metadata.insert("tool_parser_id".to_string(), value.to_string());
            }
            ("manifest_metadata", "chat_template_hash", _) => {
                metadata.insert(
                    "chat_template_hash_requirement".to_string(),
                    "required_after_rendering".to_string(),
                );
            }
            ("promotion_gate", "parser_concurrency_policy", Some(value)) => {
                metadata.insert("parser_concurrency_policy".to_string(), value.to_string());
            }
            _ => {}
        }
    }
    metadata
}

fn dataset_format_for_tool_format(tool_format: &str) -> &'static str {
    match tool_format {
        "native" => "messages_with_tool_calls",
        "json" => "harn_text_tool_calls_json_fences",
        "text" => "harn_text_tool_calls_heredoc",
        _ => "harn_text_tool_calls",
    }
}

fn required_columns_for_dataset(dataset_format: &str) -> Vec<String> {
    match dataset_format {
        "messages_with_tool_calls" => vec!["messages".to_string(), "tools".to_string()],
        _ => vec![
            "messages".to_string(),
            "tools".to_string(),
            "assistant_tool_text".to_string(),
        ],
    }
}

fn validation_steps_for_dataset(dataset_format: &str) -> Vec<String> {
    match dataset_format {
        "messages_with_tool_calls" => vec![
            "validate every assistant message has structured tool_calls or plain text, never both"
                .to_string(),
            "validate every tool role message is paired with an assistant tool call".to_string(),
            "validate every example carries the exact tool schemas exposed at inference"
                .to_string(),
        ],
        _ => vec![
            "parse assistant_tool_text with Harn's text tool-call parser".to_string(),
            "validate tool names and arguments against the inference tool schemas".to_string(),
            "reject prose around tool calls unless the target parser explicitly accepts it"
                .to_string(),
        ],
    }
}

pub(super) fn source_tool_format_required_for_target(tool_format: &str) -> &'static str {
    match tool_format {
        "text" => "text",
        "json" | "native" => "json",
        _ => "auto",
    }
}

fn training_notes(tool_format: &str) -> Vec<String> {
    match tool_format {
        "native" => vec![
            "train chat examples in the model's native tools/messages shape".to_string(),
            "preserve a tools/schema column so inference and training share one contract"
                .to_string(),
        ],
        "json" => vec![
            "train assistant completions to emit Harn fenced-JSON text tool calls".to_string(),
            "keep assistant-only loss so prompts and tool results are not learned as targets"
                .to_string(),
        ],
        "text" => vec![
            "train assistant completions to emit Harn heredoc-capable text tool calls".to_string(),
            "keep assistant-only loss so prompts and tool results are not learned as targets"
                .to_string(),
        ],
        _ => vec!["train against the route's validated tool-call format".to_string()],
    }
}

fn trainer_contract_for_dataset(
    dataset_format: &str,
    tool_format: &str,
    trainer: &str,
    modules_to_save: &[String],
) -> Vec<String> {
    let machine_contract = lora_training_contract(dataset_format, tool_format, modules_to_save);
    let peft_policy = &machine_contract.peft_save_policy;
    let mut contract = vec![
        "use TRL SFTTrainer with PEFT LoRA/QLoRA; keep the base weights frozen and save only adapter artifacts".to_string(),
        "set assistant_only_loss=true so prompts, tool schemas, and tool observations are context rather than targets".to_string(),
        "verify the tokenizer chat template emits assistant generation masks before trusting assistant_only_loss".to_string(),
        "keep packing=false unless a boundary-aware packer preserves complete tool-call/tool-result pairs".to_string(),
        format!(
            "set PEFT modules_to_save={}; keep embedding/lm_head saves explicit in the manifest",
            if peft_policy.modules_to_save.is_empty() {
                "[]".to_string()
            } else {
                format!("{:?}", peft_policy.modules_to_save)
            }
        ),
        "if embed_tokens or lm_head are saved for a tied-output base, verify PEFT weight tying before merge or keep the adapter unmerged".to_string(),
    ];
    match dataset_format {
        "messages_with_tool_calls" => {
            contract.push(
                "each record must include messages plus a tools column; assistant tool_calls and tool role messages stay paired".to_string(),
            );
        }
        _ => {
            contract.push(
                "each record must include messages, tools, and assistant_tool_text; parse assistant_tool_text with Harn before tokenization".to_string(),
            );
        }
    }
    if matches!(tool_format, "text" | "json") {
        contract.push(
            "do not train provider-native tool tags for Harn text/json routes; Harn remains the parser at inference".to_string(),
        );
    }
    match trainer {
        "unsloth_sft" => {
            contract.push(
                "use Unsloth only as the trainer backend; Harn remains the authority for export, manifest, eval, and serving contracts".to_string(),
            );
            contract.push(
                "enable Unsloth gradient checkpointing for long tool-call transcripts and keep packing disabled unless the packer preserves tool boundaries".to_string(),
            );
            contract.push(
                "record torch/CUDA, tokenizer class, and chat-template hash in `harn models lora manifest --target-metadata` after training".to_string(),
            );
        }
        "external_sft_trainer" => {
            contract.push(
                "external trainers must reproduce the Harn trainer contract exactly and stamp their backend/version in the LoRA manifest".to_string(),
            );
        }
        _ => {
            contract.push(
                "use the stock TRL/PEFT backend unless a named trainer backend is recorded in the manifest".to_string(),
            );
        }
    }
    contract.push(format!(
        "machine contract: mask={} packing={} parser_owner={} split={}",
        machine_contract.assistant_mask_policy,
        machine_contract.packing_policy,
        machine_contract.tool_parser_owner,
        machine_contract.dataset_split_policy
    ));
    contract
}

pub(super) fn lora_training_contract(
    dataset_format: &str,
    tool_format: &str,
    modules_to_save: &[String],
) -> LoraTrainingContract {
    LoraTrainingContract {
        schema_version: LORA_TRAINING_CONTRACT_SCHEMA_VERSION,
        loss_scope: "assistant_tool_calls".to_string(),
        assistant_mask_policy: "require_chat_template_generation_masks".to_string(),
        packing_policy: "disabled_unless_boundary_aware_tool_pack_pairs".to_string(),
        tool_parser_owner: tool_parser_owner_for_format(tool_format).to_string(),
        dataset_format: dataset_format.to_string(),
        dataset_split_policy: "train_tune_holdout_disjoint_no_eval_holdout_training".to_string(),
        peft_save_policy: peft_save_policy(modules_to_save),
        required_example_metadata: vec![
            "dataset_format".to_string(),
            "source_tool_format".to_string(),
            "source_record_id".to_string(),
            "source_transcript_id".to_string(),
            "teacher_model".to_string(),
            "teacher_provider".to_string(),
            "target_base_model".to_string(),
            "target_tool_format".to_string(),
            "tool_schema_hash".to_string(),
            "prompt_template_hash".to_string(),
            "split".to_string(),
            "license".to_string(),
            "lora_contract_id".to_string(),
            "lora_target".to_string(),
        ],
    }
}

fn peft_save_policy(modules_to_save: &[String]) -> PeftSavePolicy {
    let saves_embeddings = modules_to_save
        .iter()
        .any(|module| matches!(module.as_str(), "embed_tokens" | "lm_head"));
    PeftSavePolicy {
        schema_version: LORA_PEFT_SAVE_POLICY_SCHEMA_VERSION,
        modules_to_save: modules_to_save.to_vec(),
        save_embedding_layers: if saves_embeddings {
            "explicit_modules_to_save_declared".to_string()
        } else {
            "disabled_unless_tokenizer_vocab_changed".to_string()
        },
        tied_embedding_policy: if saves_embeddings {
            "verify_tied_embed_tokens_lm_head_remain_tied_before_merge_or_keep_adapter_unmerged"
                .to_string()
        } else {
            "no_embedding_or_lm_head_adapter_weights_expected".to_string()
        },
        requires_weight_tying_check: saves_embeddings,
        notes: vec![
            "default tool-calling LoRA/QLoRA adapters save only adapter weights".to_string(),
            "declare embed_tokens or lm_head only when tokenizer vocabulary or output-head training requires it".to_string(),
            "record tokenizer resize and weight-tying evidence in target metadata when saving embedding/head modules".to_string(),
        ],
    }
}

fn lora_contract_id(
    base_model: &str,
    provider: &str,
    harn_tool_format: &str,
    dataset_format: &str,
    chat_template: Option<&str>,
    modules_to_save: &[String],
) -> Result<String, String> {
    let input = LoraContractHashInput {
        schema_version: LORA_CONTRACT_HASH_SCHEMA_VERSION,
        base_model,
        provider,
        harn_tool_format,
        dataset_format,
        chat_template,
        modules_to_save,
    };
    let bytes = serde_json::to_vec(&input)
        .map_err(|error| format!("failed to render LoRA contract hash input: {error}"))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

fn lora_contract_report(
    contract_id: String,
    base_model: &str,
    provider: &str,
    harn_tool_format: &str,
    dataset_format: &str,
    chat_template: Option<String>,
    modules_to_save: &[String],
) -> LoraContractReport {
    LoraContractReport {
        schema_version: LORA_CONTRACT_SCHEMA_VERSION,
        id: contract_id,
        base_model: base_model.to_string(),
        provider: provider.to_string(),
        harn_tool_format: harn_tool_format.to_string(),
        dataset_format: dataset_format.to_string(),
        chat_template,
        training_contract: lora_training_contract(
            dataset_format,
            harn_tool_format,
            modules_to_save,
        ),
    }
}

fn tool_parser_owner_for_format(tool_format: &str) -> &'static str {
    match tool_format {
        "native" => "provider_tokenizer_runtime",
        "text" | "json" => "harn_text_tool_parser",
        _ => "catalog_validated_route",
    }
}

pub(super) fn lora_adapter_binding(provider_supports_lora_launch: bool) -> &'static str {
    if provider_supports_lora_launch {
        "runtime_lora_adapter"
    } else {
        "external_runtime_or_merged_adapter"
    }
}

pub(super) fn lora_modules_value_format(
    local_runtime: Option<&harn_vm::llm_config::LocalRuntimeDef>,
) -> String {
    local_runtime
        .and_then(|runtime| runtime.lora_modules_value_format.as_deref())
        .unwrap_or("name_path")
        .to_string()
}

fn serving_recipe(
    base_model: &str,
    provider: &str,
    request_model: &str,
    adapter_name: &str,
    tool_format: &str,
    dataset_format: &str,
    provider_supports_lora_launch: bool,
    lora_module_value_format: &str,
) -> ServingRecipe {
    let adapter_binding = lora_adapter_binding(provider_supports_lora_launch).to_string();
    let mut runtime_notes = Vec::new();
    if provider_supports_lora_launch {
        runtime_notes.push(
            "serve the base model once and select the LoRA adapter per request model name"
                .to_string(),
        );
        runtime_notes.push(
            "keep adapter names stable across train, inspect, local launch, and eval reports"
                .to_string(),
        );
    } else {
        runtime_notes.push(
            "register the adapter in the external runtime or merge it only after promotion gates pass"
                .to_string(),
        );
        runtime_notes.push(
            "record the runtime-specific adapter binding in the export manifest metadata"
                .to_string(),
        );
    }
    runtime_notes.push(
        "do not change the tool-call format between dataset export, serving, and evaluation"
            .to_string(),
    );
    runtime_notes.extend(tool_call_serving_notes(base_model, provider, tool_format));
    let serving_requirements = tool_call_serving_requirements(base_model, provider, tool_format);
    ServingRecipe {
        request_model: request_model.to_string(),
        adapter_name: adapter_name.to_string(),
        base_model: base_model.to_string(),
        provider: provider.to_string(),
        adapter_binding,
        lora_module_value_format: lora_module_value_format.to_string(),
        tool_format: tool_format.to_string(),
        dataset_format: dataset_format.to_string(),
        serving_requirements,
        runtime_notes,
        promotion_gates: vec![
            "inspect the adapter against the exact served base model before launch".to_string(),
            "run base-versus-adapter tool-call evals with the same request model selector"
                .to_string(),
            "keep a rollback path to the base route or previous adapter revision".to_string(),
        ],
    }
}

fn tool_call_serving_requirements(
    base_model: &str,
    provider: &str,
    tool_format: &str,
) -> Vec<ServingRequirement> {
    let mut requirements = Vec::new();
    if matches!(tool_format, "text" | "json") {
        requirements.push(serving_requirement(
            "parser_owner",
            "tool_call_parser",
            Some("harn_text_tool_parser"),
            true,
            "Harn parses text/json tool calls for this route.",
        ));
        requirements.push(serving_requirement(
            "provider_native_tool_parser",
            "native_tool_parser_mode",
            Some("disabled_unless_proxy_maps_to_harn_text"),
            true,
            "Provider-native tool parsers must not reinterpret Harn text tool calls.",
        ));
        return requirements;
    }

    if tool_format != "native" {
        requirements.push(serving_requirement(
            "parser_owner",
            "tool_call_parser",
            Some("catalog_validated_route"),
            true,
            "Resolve the effective route before serving.",
        ));
        return requirements;
    }

    requirements.push(serving_requirement(
        "parser_owner",
        "tool_call_parser",
        Some("provider_tokenizer_runtime"),
        true,
        "Native tool routes rely on the provider/tokenizer runtime parser.",
    ));

    if provider == "vllm" {
        requirements.push(serving_requirement(
            "server_flag",
            "--enable-auto-tool-choice",
            None,
            true,
            "vLLM native tool routes need automatic tool-call extraction enabled.",
        ));
        if is_functiongemma_route(base_model, "", "") {
            requirements.push(serving_requirement(
                "server_flag",
                "--tool-call-parser",
                Some("functiongemma"),
                true,
                "FunctionGemma native routes need the matching vLLM parser.",
            ));
            requirements.push(serving_requirement(
                "chat_template",
                "chat_template",
                Some("functiongemma_control_tokens"),
                true,
                "Training and serving must share the FunctionGemma control-token template.",
            ));
            requirements.push(serving_requirement(
                "stop_sequence",
                "inference_stop_sequence",
                Some("<start_function_response>"),
                true,
                "Function response control tokens must not be generated as ordinary text.",
            ));
        } else if is_gemma4_route(base_model, "", "") {
            requirements.push(serving_requirement(
                "server_flag",
                "--tool-call-parser",
                Some("gemma4"),
                true,
                "Gemma 4 native routes need vLLM's Gemma 4 tool-call parser.",
            ));
            requirements.push(serving_requirement(
                "server_flag",
                "--reasoning-parser",
                Some("gemma4"),
                false,
                "Required when Gemma 4 thinking traces are enabled; keep explicit in launch manifests.",
            ));
            requirements.push(serving_requirement(
                "chat_template",
                "chat_template",
                Some("examples/tool_chat_template_gemma4.jinja"),
                true,
                "Gemma 4 tool calling depends on the vLLM-compatible tool chat template.",
            ));
            requirements.push(serving_requirement(
                "manifest_metadata",
                "tool_parser_id",
                Some("gemma4"),
                true,
                "Persist the parser id so inspect can detect serving-route drift.",
            ));
            requirements.push(serving_requirement(
                "manifest_metadata",
                "chat_template_hash",
                None,
                true,
                "Persist the chat-template hash so training and serving can be compared.",
            ));
            requirements.push(serving_requirement(
                "promotion_gate",
                "parser_concurrency_policy",
                Some("serialize_validation_or_pin_parser_version"),
                true,
                "Promotion must prove the exact parser/template route used in serving.",
            ));
        } else {
            requirements.push(serving_requirement(
                "server_flag",
                "--tool-call-parser",
                Some("model_family_parser"),
                true,
                "vLLM native tool routes need the parser matching the model family.",
            ));
            requirements.push(serving_requirement(
                "chat_template",
                "chat_template",
                Some("model_family_tool_template"),
                true,
                "Training and serving must share the model-family tool template.",
            ));
        }
    }

    requirements
}

fn serving_requirement(
    kind: &str,
    name: &str,
    value: Option<&str>,
    required: bool,
    reason: &str,
) -> ServingRequirement {
    ServingRequirement {
        kind: kind.to_string(),
        name: name.to_string(),
        value: value.map(str::to_string),
        required,
        reason: reason.to_string(),
    }
}

fn tool_call_serving_notes(base_model: &str, provider: &str, tool_format: &str) -> Vec<String> {
    let mut notes = Vec::new();
    if matches!(tool_format, "text" | "json") {
        notes.push(
            "serve the adapter as a text-channel route: Harn owns tool-call parsing for this plan"
                .to_string(),
        );
        notes.push(
            "keep provider-native tool parsers disabled unless the proxy maps them back to Harn text tool calls"
                .to_string(),
        );
        return notes;
    }
    if tool_format != "native" {
        return notes;
    }

    notes.push(
        "prefer schema-constrained or strict tool calling during serving and eval when the runtime supports it"
            .to_string(),
    );
    if provider == "vllm" {
        notes.push(
            "for vLLM native tools, serve with --enable-auto-tool-choice and the model family's matching --tool-call-parser/chat-template"
                .to_string(),
        );
    }
    if is_functiongemma_route(base_model, "", "") {
        notes.push(
            "FunctionGemma routes need the functiongemma parser/chat template and <start_function_response> stop handling"
                .to_string(),
        );
    } else if is_gemma4_route(base_model, "", "") {
        notes.push(
            "Gemma 4 native routes must keep the tokenizer/provider tool declaration, call, and response template identical between training and serving"
                .to_string(),
        );
        if provider == "vllm" {
            notes.push(
                "serialize Gemma 4 native-tool validation traffic or pin a vLLM release whose gemma4 parser is concurrency-safe before promotion"
                    .to_string(),
            );
            notes.push(
                "record the vLLM gemma4 tool-call parser and chat-template revision in the LoRA manifest"
                    .to_string(),
            );
        }
    }
    notes
}

fn teacher_report(selector: &str) -> TeacherReport {
    let resolved = harn_vm::llm_config::resolve_model_info(selector);
    let provider = resolved.provider.clone();
    TeacherReport {
        selector: selector.to_string(),
        id: resolved.id.clone(),
        provider,
        resolved_alias: resolved.alias,
        tool_format: harn_vm::llm_config::default_tool_format(&resolved.id, &resolved.provider),
        family: resolved.family,
        lineage: resolved.lineage,
    }
}

fn corpus_refresh_recipe(
    strategy: &str,
    teacher: Option<&TeacherReport>,
    tool_format: &str,
    dataset_format: &str,
) -> CorpusRefreshRecipe {
    let teacher_required = matches!(strategy, "refresh" | "distill");
    let mut generation_notes = match strategy {
        "refresh" => vec![
            "use the teacher to repair or extend existing corpus records; preserve stable ids for unchanged examples".to_string(),
            "write new examples only into train/tune splits until a separate holdout review promotes them".to_string(),
        ],
        "distill" => vec![
            "use the teacher to generate synthetic task/tool/result trajectories from frozen tool schemas".to_string(),
            "sample single-turn and multi-turn cases separately so turn-repair behavior remains measurable".to_string(),
        ],
        _ => vec![
            "audit the supplied corpus without synthetic generation before training".to_string(),
            "prefer parser/schema fixes over adding near-duplicate examples".to_string(),
        ],
    };
    generation_notes.push(format!(
        "render every accepted example in the effective `{tool_format}` tool-call convention"
    ));
    generation_notes.push(format!(
        "store examples in `{dataset_format}` form so training and eval consume one contract"
    ));
    if let Some(teacher) = teacher {
        generation_notes.push(format!(
            "record teacher route {} via {} for every synthetic or repaired record",
            teacher.id, teacher.provider
        ));
    }
    CorpusRefreshRecipe {
        strategy: strategy.to_string(),
        teacher_required,
        teacher: teacher.cloned(),
        generation_notes,
        provenance_manifest_fields: vec![
            "source_record_id".to_string(),
            "source_transcript_id".to_string(),
            "teacher_model".to_string(),
            "teacher_provider".to_string(),
            "target_base_model".to_string(),
            "target_tool_format".to_string(),
            "tool_schema_hash".to_string(),
            "prompt_template_hash".to_string(),
            "split".to_string(),
            "license".to_string(),
        ],
        hard_negative_slices: vec![
            "wrong-tool disambiguation under similar schemas".to_string(),
            "malformed-call repair without executing unsafe arguments".to_string(),
            "permission-denied or no-write tool outcomes".to_string(),
            "tool-result follow-up after partial or empty results".to_string(),
            "multi-turn correction after stale or contradictory observations".to_string(),
        ],
        acceptance_gates: vec![
            "target parser accepts every assistant tool-call target".to_string(),
            "tool names and arguments validate against the frozen inference schemas".to_string(),
            "dedupe by normalized tool name, arguments, and outcome class".to_string(),
            "train/tune/holdout splits stay disjoint from Harn and Burin eval holdouts".to_string(),
            "base-versus-adapter eval runs on identical cases before promotion".to_string(),
        ],
        model_aware_selection: model_aware_selection_recipe(strategy, tool_format, dataset_format),
    }
}

fn model_aware_selection_recipe(
    strategy: &str,
    tool_format: &str,
    dataset_format: &str,
) -> ModelAwareSelectionRecipe {
    let parser_signal = if matches!(tool_format, "text" | "json") {
        "Harn text-parser failure class and repair distance"
    } else {
        "native tool-call schema validation error class"
    };
    let generation_scope = match strategy {
        "refresh" => "score existing and teacher-repaired records before adding them to train/tune",
        "distill" => {
            "score synthetic teacher trajectories before accepting them into a generated corpus"
        }
        _ => "score the supplied corpus and report gaps without generating new records",
    };
    ModelAwareSelectionRecipe {
        objective: "train on examples that expose the target base model's tool-use failure modes, not on near-duplicate syntax drills"
            .to_string(),
        difficulty_signals: vec![
            "target base-model outcome bucket: solved, missing_call, malformed_call, wrong_tool, bad_arguments, unsafe_or_unpermitted_call, premature_final_answer"
                .to_string(),
            parser_signal.to_string(),
            "tool schema overlap and argument-shape ambiguity for wrong-tool disambiguation"
                .to_string(),
            "turn-repair state: first-call, tool-result follow-up, permission denial, stale observation, or corrective retry"
                .to_string(),
            format!("dataset contract `{dataset_format}` sequence-fit and assistant-mask validity"),
        ],
        sampling_policy: vec![
            generation_scope.to_string(),
            "prioritize medium-difficulty candidates the base model nearly solves; cap already-solved and impossible examples"
                .to_string(),
            "balance single-turn calls, multi-turn repair, no-tool final answers, and permission/no-write outcomes"
                .to_string(),
            "dedupe by normalized tool name, arguments, outcome class, language, and task type before training"
                .to_string(),
        ],
        refinement_loop: vec![
            "round 0: preflight the frozen corpus, schemas, template, and split manifest"
                .to_string(),
            "round N: run the current base-or-adapter route on tune cases, bucket failures, then add or reweight only parser-valid teacher repairs"
                .to_string(),
            "keep holdout frozen before the first scoring pass; never recycle holdout failures into train/tune without changing the split id"
                .to_string(),
            "rerun base-versus-adapter eval on identical cases after each accepted refresh round"
                .to_string(),
        ],
        stop_conditions: vec![
            "no new failure bucket improves after a refresh round".to_string(),
            "paired tune lift is positive but holdout remains unmeasured; stop before promotion"
                .to_string(),
            "adapter regresses non-tool chat, safe refusal, or no-write cases".to_string(),
        ],
    }
}

pub(super) fn lora_evaluation_recipe(
    contract_id: &str,
    base_model: &str,
    provider: &str,
    request_model: &str,
    tool_format: &str,
    eval_dataset: &str,
    eval_command: Vec<String>,
) -> EvaluationRecipe {
    let parser_metric = if matches!(tool_format, "text" | "json") {
        "Harn text parser acceptance rate"
    } else {
        "native tool-call schema acceptance rate"
    };
    let minimum_trials = 5;
    let comparison_baseline =
        "same base model, provider, tool format, prompt template, and tool schemas without the adapter"
            .to_string();
    let required_metrics = vec![
        "exact tool-name + argument match rate".to_string(),
        parser_metric.to_string(),
        "malformed-call and prose-only failure rate".to_string(),
        "wrong-tool false positive rate".to_string(),
        "latency and cost per solved tool-call case".to_string(),
    ];
    let gates = vec![
        "compare base versus adapter on identical tool-call cases".to_string(),
        "require a positive paired lift before promotion; inconclusive movement stays experimental"
            .to_string(),
        "require zero contract-id drift between export manifest, adapter metadata, and served route"
            .to_string(),
        "require no regression on non-tool chat smoke prompts".to_string(),
    ];
    let evidence_contract = lora_promotion_evidence_contract(PromotionEvidenceInput {
        contract_id,
        base_model,
        provider,
        request_model,
        tool_format,
        eval_dataset,
        minimum_trials,
        required_metrics: &required_metrics,
        gates: &gates,
    });
    EvaluationRecipe {
        holdout_policy:
            "keep train/tune/holdout splits disjoint; never train on Harn eval fixtures".to_string(),
        minimum_trials,
        comparison_baseline,
        required_metrics,
        gates,
        evidence_contract,
        eval_command,
    }
}

fn lora_promotion_evidence_contract(
    input: PromotionEvidenceInput<'_>,
) -> PromotionEvidenceContract {
    let required_probe_cases = lora_required_probe_cases(input.tool_format);
    let promotion_id = lora_promotion_id(&input, &required_probe_cases);
    PromotionEvidenceContract {
        schema_version: LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION,
        promotion_id,
        lora_contract_id: input.contract_id.to_string(),
        base_route: PromotionRoute {
            role: "base".to_string(),
            provider: input.provider.to_string(),
            model: input.base_model.to_string(),
            tool_format: input.tool_format.to_string(),
        },
        adapter_route: PromotionRoute {
            role: "adapter".to_string(),
            provider: input.provider.to_string(),
            model: input.request_model.to_string(),
            tool_format: input.tool_format.to_string(),
        },
        eval_dataset: input.eval_dataset.to_string(),
        minimum_trials: input.minimum_trials,
        required_receipts: vec![
            "lora_preflight_report".to_string(),
            "lora_export_manifest".to_string(),
            "lora_adapter_manifest".to_string(),
            "lora_inspect_report".to_string(),
            "tool_probe_receipt".to_string(),
            "promotion_probe_matrix_receipt".to_string(),
            "base_eval_receipt".to_string(),
            "adapter_eval_receipt".to_string(),
        ],
        required_probe_cases,
        optional_batch_receipts: vec![
            "harn.model_batch_manifest".to_string(),
            "harn.model_batch_prepare_receipt".to_string(),
            "harn.model_batch_submission_receipt".to_string(),
            "harn.model_batch_status_receipt".to_string(),
            "harn.model_batch_results_receipt".to_string(),
        ],
        batch_ready: PromotionBatchReady {
            workload: "eval".to_string(),
            group_by: vec![
                "provider".to_string(),
                "model".to_string(),
                "tool_format".to_string(),
                "lora_contract_id".to_string(),
                "promotion_id".to_string(),
            ],
            request_row_contract: vec![
                "custom_id".to_string(),
                "provider".to_string(),
                "model".to_string(),
                "tool_format".to_string(),
                "messages".to_string(),
                "tools".to_string(),
                "metadata.promotion_id".to_string(),
                "metadata.lora_contract_id".to_string(),
                "metadata.route_role".to_string(),
                "metadata.case_id".to_string(),
            ],
            manifest_command: vec![
                "harn".to_string(),
                "models".to_string(),
                "batch".to_string(),
                "manifest".to_string(),
                "--workload".to_string(),
                "eval".to_string(),
                "--tool-format".to_string(),
                input.tool_format.to_string(),
                "--requests".to_string(),
                "PROMOTION_REQUESTS.jsonl".to_string(),
                "--out".to_string(),
                "PROMOTION_BATCH.manifest.json".to_string(),
                "--id-prefix".to_string(),
                "lora-promotion".to_string(),
            ],
        },
        acceptance: PromotionAcceptance {
            required_metrics: input.required_metrics.to_vec(),
            gates: input.gates.to_vec(),
        },
    }
}

fn lora_required_probe_cases(tool_format: &str) -> Vec<PromotionProbeCase> {
    let tool_surface = match tool_format {
        "native" => "provider-native structured tool call",
        "json" => "Harn fenced-JSON text tool-call block accepted by the parser",
        "text" => "Harn heredoc-capable text tool-call block accepted by the parser",
        _ => "Harn text tool-call block accepted by the catalog-selected parser",
    };
    vec![
        PromotionProbeCase {
            id: "sequential_tool_call".to_string(),
            requirement: "always".to_string(),
            expected: format!(
                "adapter-loaded route emits exactly one valid {tool_surface} with the requested tool name and arguments"
            ),
            receipt: "tool_probe_receipt.sequential_tool_call".to_string(),
            rationale: "catches the primary one-tool happy path before aggregate eval scores can hide parser drift"
                .to_string(),
        },
        PromotionProbeCase {
            id: "parallel_tool_calls".to_string(),
            requirement: "required_when_route_supports_parallel_tool_calls_else_not_applicable_receipt"
                .to_string(),
            expected:
                "adapter-loaded route emits distinct tool calls with stable ids, names, and arguments when the route advertises parallel tools"
                    .to_string(),
            receipt: "tool_probe_receipt.parallel_tool_calls".to_string(),
            rationale:
                "prevents a LoRA from passing on sequential fixtures while breaking the advertised parallel contract"
                    .to_string(),
        },
        PromotionProbeCase {
            id: "no_tool_answer".to_string(),
            requirement: "always".to_string(),
            expected: "adapter-loaded route answers a non-tool prompt without emitting any tool call"
                .to_string(),
            receipt: "tool_probe_receipt.no_tool_answer".to_string(),
            rationale: "guards against over-triggered tool calls from narrow tool-call fine-tuning"
                .to_string(),
        },
        PromotionProbeCase {
            id: "unavailable_tool_repair".to_string(),
            requirement: "always".to_string(),
            expected:
                "adapter-loaded route recovers when the requested tool is absent instead of fabricating an unavailable tool call"
                    .to_string(),
            receipt: "tool_probe_receipt.unavailable_tool_repair".to_string(),
            rationale:
                "keeps tool selection grounded in the served schema rather than the training corpus inventory"
                    .to_string(),
        },
        PromotionProbeCase {
            id: "multi_turn_tool_result_continuation".to_string(),
            requirement: "always".to_string(),
            expected:
                "adapter-loaded route consumes a tool result and continues the same task without repeating or orphaning the prior call"
                    .to_string(),
            receipt: "tool_probe_receipt.multi_turn_tool_result_continuation".to_string(),
            rationale: "covers transcript lifecycle behavior that single-turn tool probes cannot observe"
                .to_string(),
        },
        PromotionProbeCase {
            id: "serving_concurrency_probe".to_string(),
            requirement: "required_for_adapter_loaded_serving_else_not_applicable_receipt"
                .to_string(),
            expected:
                "adapter-loaded serving route preserves adapter binding, parser mode, and request ids across concurrent probe requests"
                    .to_string(),
            receipt: "tool_probe_receipt.serving_concurrency_probe".to_string(),
            rationale: "separates offline adapter quality from serving-path adapter and parser isolation"
                .to_string(),
        },
    ]
}

struct PromotionEvidenceInput<'a> {
    contract_id: &'a str,
    base_model: &'a str,
    provider: &'a str,
    request_model: &'a str,
    tool_format: &'a str,
    eval_dataset: &'a str,
    minimum_trials: u64,
    required_metrics: &'a [String],
    gates: &'a [String],
}

fn lora_promotion_id(
    input: &PromotionEvidenceInput<'_>,
    required_probe_cases: &[PromotionProbeCase],
) -> String {
    let mut hasher = Sha256::new();
    for part in [
        "harn_lora_promotion_v2",
        input.contract_id,
        input.base_model,
        input.provider,
        input.request_model,
        input.tool_format,
        input.eval_dataset,
        &input.minimum_trials.to_string(),
    ] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    for metric in input.required_metrics {
        hasher.update(metric.as_bytes());
        hasher.update([0]);
    }
    for gate in input.gates {
        hasher.update(gate.as_bytes());
        hasher.update([0]);
    }
    let probe_case_bytes = serde_json::to_vec(required_probe_cases)
        .expect("promotion probe cases are JSON-serializable");
    hasher.update(probe_case_bytes);
    hasher.update([0]);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn template_recipe_for_route(
    model_id: &str,
    family: &str,
    lineage: &str,
    tool_format: &str,
) -> TemplateRecipe {
    if tool_format == "native" && is_functiongemma_route(model_id, family, lineage) {
        return TemplateRecipe {
            name: "functiongemma_control_tokens".to_string(),
            source: "FunctionGemma declaration/call/response control-token template".to_string(),
            supervised_target: "model turn containing function-call control-token blocks"
                .to_string(),
            requirements: vec![
                "render function declarations, calls, and responses with FunctionGemma control tokens"
                    .to_string(),
                "treat <start_function_response> as an inference stop sequence".to_string(),
                "preserve string-value escaping with the model's escape delimiter".to_string(),
            ],
            stop_sequences: vec!["<start_function_response>".to_string()],
            notes: vec![
                "FunctionGemma is a specialized text-only function-calling model; do not mix this template with Harn <tool_call> text records"
                    .to_string(),
                "keep single-turn and multi-turn examples separated in eval so specialization does not hide turn-repair regressions"
                    .to_string(),
            ],
        };
    }
    if tool_format == "native" && is_gemma4_route(model_id, family, lineage) {
        return TemplateRecipe {
            name: "gemma4_native_function_calling".to_string(),
            source: "Gemma 4 tokenizer/provider native function-calling chat template".to_string(),
            supervised_target: "assistant messages with native tool_calls plus paired tool role results"
                .to_string(),
            requirements: vec![
                "use messages plus tools JSON schemas; let the tokenizer/provider render the Gemma 4 tool declaration syntax"
                    .to_string(),
                "train against the same native tool-call shape used at inference".to_string(),
                "do not include Harn <tool_call> text blocks in native Gemma 4 examples".to_string(),
            ],
            stop_sequences: Vec::new(),
            notes: vec![
                "Gemma 4 has native function-calling support, but local runtimes may still be catalog-steered to Harn text/json formats"
                    .to_string(),
                "if the route is served through Harn text/json, prefer the Harn template plan over the native Gemma 4 template"
                    .to_string(),
            ],
        };
    }
    match tool_format {
        "native" => TemplateRecipe {
            name: "native_messages_with_tools".to_string(),
            source: "tokenizer/provider chat template with tool schemas".to_string(),
            supervised_target: "assistant tool_calls and final assistant messages".to_string(),
            requirements: vec![
                "store examples as messages plus a tools column containing JSON schemas".to_string(),
                "represent tool results as tool role messages paired to assistant tool calls".to_string(),
                "verify the tokenizer chat template supports tool use before training".to_string(),
            ],
            stop_sequences: Vec::new(),
            notes: vec![
                "native adapters are portable only across runtimes that preserve the same chat template and tool schema rendering"
                    .to_string(),
            ],
        },
        "json" => TemplateRecipe {
            name: "harn_text_tool_calls_json_fences".to_string(),
            source: "Harn text tool-call parser using JSON object bodies".to_string(),
            supervised_target: "assistant_tool_text containing <tool_call>{\"name\":...,\"arguments\":...}</tool_call>"
                .to_string(),
            requirements: vec![
                "parse every assistant_tool_text example with Harn before training".to_string(),
                "keep tool definitions in the tools column and keep serialized calls byte-stable"
                    .to_string(),
                "reject markdown fences or model-native tool tags inside <tool_call> blocks"
                    .to_string(),
            ],
            stop_sequences: vec!["</tool_call>".to_string()],
            notes: vec![
                "this is the right target when the catalog steers a model to Harn's JSON text tool convention"
                    .to_string(),
            ],
        },
        "text" => TemplateRecipe {
            name: "harn_text_tool_calls_heredoc".to_string(),
            source: "Harn text tool-call parser using name({ ... }) and heredoc bodies".to_string(),
            supervised_target: "assistant_tool_text containing Harn text/heredoc <tool_call> blocks"
                .to_string(),
            requirements: vec![
                "parse every assistant_tool_text example with Harn before training".to_string(),
                "preserve heredoc boundaries for multiline edit/scaffold arguments".to_string(),
                "reject JSON object tool-call bodies unless the record declares the json lane"
                    .to_string(),
            ],
            stop_sequences: vec!["</tool_call>".to_string()],
            notes: vec![
                "this is the most direct adapter target for Burin's text tool-calling corpus"
                    .to_string(),
            ],
        },
        _ => TemplateRecipe {
            name: "route_validated_tool_template".to_string(),
            source: "catalog-validated route tool-call convention".to_string(),
            supervised_target: "assistant tool-call target selected by the effective route".to_string(),
            requirements: vec!["resolve the effective tool format before exporting examples".to_string()],
            stop_sequences: Vec::new(),
            notes: vec!["keep training and inference on the same route convention".to_string()],
        },
    }
}

fn is_functiongemma_route(model_id: &str, family: &str, lineage: &str) -> bool {
    route_key(model_id, family, lineage).contains("functiongemma")
}

fn is_gemma4_route(model_id: &str, family: &str, lineage: &str) -> bool {
    let key = route_key(model_id, family, lineage);
    key.contains("gemma-4") || key.contains("gemma4")
}

fn route_key(model_id: &str, family: &str, lineage: &str) -> String {
    format!("{model_id} {family} {lineage}").to_ascii_lowercase()
}

fn plan_warnings(
    provider: &str,
    decision: &harn_vm::llm::capabilities::ToolFormatDecision,
    provider_supports_lora_launch: bool,
    native_tools: bool,
    requested_tool_format: &str,
    requested_corpus_strategy: &str,
    effective_corpus_strategy: &str,
    teacher: Option<&TeacherReport>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(correction) = &decision.correction {
        warnings.push(correction.clone());
    }
    if requested_tool_format == "native" && decision.effective != "native" {
        warnings.push("native tool training requested but the catalog steered this route to a text-channel format".to_string());
    }
    if decision.effective == "native" && !native_tools {
        warnings.push(
            "effective tool format is native, but this route does not advertise native tools; use auto/text/json unless the serving proxy supplies native tools"
                .to_string(),
        );
    }
    if !provider_supports_lora_launch {
        warnings.push(format!(
            "provider {provider} does not declare local-runtime LoRA launch flags; plan still describes training and eval, but launch must be external"
        ));
    }
    if matches!(effective_corpus_strategy, "refresh" | "distill") && teacher.is_none() {
        warnings.push(format!(
            "corpus strategy {effective_corpus_strategy} needs --teacher to generate or repair examples"
        ));
    }
    if requested_corpus_strategy == "audit-only" && teacher.is_some() {
        warnings.push(
            "--teacher was supplied but corpus strategy is audit-only; teacher metadata is recorded but generation stays disabled"
                .to_string(),
        );
    }
    warnings
}

#[derive(Debug, Serialize)]
struct LoraInspectReport {
    ok: bool,
    base: BaseModelReport,
    adapter: AdapterReport,
    contract: Option<InspectContractReport>,
    compatibility: CompatibilityReport,
    tool_calling: ToolCallingReport,
    serving: InspectServingReport,
    launch: LaunchHints,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct BaseModelReport {
    selector: String,
    id: String,
    provider: String,
    resolved_alias: Option<String>,
    tool_format: String,
    tier: String,
    family: String,
    lineage: String,
    catalog_name: Option<String>,
    context_window: Option<u64>,
}

#[derive(Debug, Serialize)]
struct AdapterReport {
    input: String,
    name: String,
    local_path: Option<String>,
    exists: bool,
    config_found: bool,
    config_path: Option<String>,
    weights_found: Vec<String>,
    peft_type: Option<String>,
    task_type: Option<String>,
    base_model_name_or_path: Option<String>,
    rank: Option<u64>,
    lora_alpha: Option<f64>,
    target_modules: Vec<String>,
    modules_to_save: Vec<String>,
    contract_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct InspectContractReport {
    manifest_path: String,
    contract_id: Option<String>,
    adapter_contract_id: Option<String>,
    status: ContractCheckStatus,
    base_model_match: BaseModelMatch,
    provider_matches: bool,
    tool_format_matches: bool,
    adapter_name_matches: Option<bool>,
    modules_to_save_matches: Option<bool>,
    require_adapter_contract_id: bool,
    manifest: InspectContractManifest,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct InspectContractManifest {
    base_model: Option<String>,
    provider: Option<String>,
    harn_tool_format: Option<String>,
    dataset_format: Option<String>,
    chat_template: Option<String>,
    modules_to_save: Option<Vec<String>>,
    adapter_name: Option<String>,
    request_model: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum ContractCheckStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Serialize)]
struct CompatibilityReport {
    base_model_match: BaseModelMatch,
    provider_supports_lora_launch: bool,
    provider_supports_lora_max_rank: bool,
    provider_lora_module_value_format: String,
}

#[derive(Debug, Serialize)]
struct ToolCallingReport {
    native_tools: bool,
    preferred_tool_format: Option<String>,
    text_tool_wire_format_supported: bool,
    structured_output_mode: String,
    recommended_endpoint: Option<String>,
}

#[derive(Debug, Serialize)]
struct LaunchHints {
    request_model: String,
    max_lora_rank: Option<u64>,
    harn_local_launch: Vec<String>,
}

#[derive(Debug, Serialize)]
struct InspectServingReport {
    request_model: String,
    base_model: String,
    provider: String,
    tool_format: String,
    lora_module_value_format: String,
    serving_requirements: Vec<ServingRequirement>,
}

#[derive(Debug, Serialize)]
struct LoraPlanReport {
    ok: bool,
    base: BaseModelReport,
    request: PlanRequest,
    tool_calling: ToolCallingReport,
    training: TrainingRecipe,
    precision: PrecisionContract,
    template: TemplateRecipe,
    data: DataRecipe,
    corpus_refresh: CorpusRefreshRecipe,
    evaluation: EvaluationRecipe,
    serving: ServingRecipe,
    launch: PlanLaunchHints,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PlanRequest {
    method: String,
    requested_tool_format: String,
    effective_tool_format: String,
    tool_format_correction: Option<String>,
    corpus: Option<String>,
    requested_corpus_strategy: String,
    effective_corpus_strategy: String,
    teacher: Option<TeacherReport>,
}

#[derive(Debug, Serialize)]
struct TrainingRecipe {
    adapter_type: String,
    trainer: String,
    rank: u32,
    alpha: u32,
    dropout: f64,
    quantization: String,
    loss_scope: String,
    packing: String,
    target_modules: Vec<String>,
    contract: LoraTrainingContract,
    trainer_contract: Vec<String>,
    notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct PrecisionContract {
    schema_version: u64,
    training_base_precision: String,
    training_compute_precision: String,
    adapter_weight_precision: String,
    serving_base_precision: String,
    serving_adapter_precision: String,
    compatibility_policy: String,
    promotion_gates: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct LoraTrainingContract {
    schema_version: u64,
    loss_scope: String,
    assistant_mask_policy: String,
    packing_policy: String,
    tool_parser_owner: String,
    dataset_format: String,
    dataset_split_policy: String,
    peft_save_policy: PeftSavePolicy,
    required_example_metadata: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct PeftSavePolicy {
    schema_version: u64,
    modules_to_save: Vec<String>,
    save_embedding_layers: String,
    tied_embedding_policy: String,
    requires_weight_tying_check: bool,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct LoraContractReport {
    schema_version: u64,
    id: String,
    base_model: String,
    provider: String,
    harn_tool_format: String,
    dataset_format: String,
    chat_template: Option<String>,
    training_contract: LoraTrainingContract,
}

#[derive(Serialize)]
struct LoraContractHashInput<'a> {
    schema_version: u64,
    base_model: &'a str,
    provider: &'a str,
    harn_tool_format: &'a str,
    dataset_format: &'a str,
    chat_template: Option<&'a str>,
    modules_to_save: &'a [String],
}

#[derive(Debug, Serialize)]
struct TemplateRecipe {
    name: String,
    source: String,
    supervised_target: String,
    requirements: Vec<String>,
    stop_sequences: Vec<String>,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DataRecipe {
    dataset_format: String,
    required_columns: Vec<String>,
    validation: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct TeacherReport {
    selector: String,
    id: String,
    provider: String,
    resolved_alias: Option<String>,
    tool_format: String,
    family: String,
    lineage: String,
}

#[derive(Debug, Serialize)]
struct CorpusRefreshRecipe {
    strategy: String,
    teacher_required: bool,
    teacher: Option<TeacherReport>,
    generation_notes: Vec<String>,
    provenance_manifest_fields: Vec<String>,
    hard_negative_slices: Vec<String>,
    acceptance_gates: Vec<String>,
    model_aware_selection: ModelAwareSelectionRecipe,
}

#[derive(Debug, Serialize)]
struct ModelAwareSelectionRecipe {
    objective: String,
    difficulty_signals: Vec<String>,
    sampling_policy: Vec<String>,
    refinement_loop: Vec<String>,
    stop_conditions: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct EvaluationRecipe {
    holdout_policy: String,
    minimum_trials: u64,
    comparison_baseline: String,
    required_metrics: Vec<String>,
    gates: Vec<String>,
    evidence_contract: PromotionEvidenceContract,
    eval_command: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PromotionEvidenceContract {
    schema_version: u64,
    promotion_id: String,
    lora_contract_id: String,
    base_route: PromotionRoute,
    adapter_route: PromotionRoute,
    eval_dataset: String,
    minimum_trials: u64,
    required_receipts: Vec<String>,
    required_probe_cases: Vec<PromotionProbeCase>,
    optional_batch_receipts: Vec<String>,
    batch_ready: PromotionBatchReady,
    acceptance: PromotionAcceptance,
}

#[derive(Clone, Debug, Serialize)]
struct PromotionProbeCase {
    id: String,
    requirement: String,
    expected: String,
    receipt: String,
    rationale: String,
}

#[derive(Debug, Serialize)]
struct PromotionRoute {
    role: String,
    provider: String,
    model: String,
    tool_format: String,
}

#[derive(Debug, Serialize)]
struct PromotionBatchReady {
    workload: String,
    group_by: Vec<String>,
    request_row_contract: Vec<String>,
    manifest_command: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PromotionAcceptance {
    required_metrics: Vec<String>,
    gates: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ServingRecipe {
    request_model: String,
    adapter_name: String,
    base_model: String,
    provider: String,
    adapter_binding: String,
    lora_module_value_format: String,
    tool_format: String,
    dataset_format: String,
    serving_requirements: Vec<ServingRequirement>,
    runtime_notes: Vec<String>,
    promotion_gates: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ServingRequirement {
    kind: String,
    name: String,
    value: Option<String>,
    required: bool,
    reason: String,
}

#[derive(Debug, Serialize)]
struct PlanLaunchHints {
    preflight_command: Vec<String>,
    export_command: Vec<String>,
    train_command: Vec<String>,
    manifest_command: Vec<String>,
    inspect_command: Vec<String>,
    local_launch_command: Vec<String>,
    tool_probe_command: Vec<String>,
    request_model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BaseModelMatch {
    Exact,
    Suffix,
    Mismatch,
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspects_local_peft_lora_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let adapter_dir = tmp.path().join("burin-tools");
        std::fs::create_dir(&adapter_dir).expect("adapter dir");
        std::fs::write(adapter_dir.join("adapter_model.safetensors"), b"stub")
            .expect("adapter weights");
        std::fs::write(
            adapter_dir.join("adapter_config.json"),
            r#"{
                "peft_type": "LORA",
                "base_model_name_or_path": "google/gemma-4-e4b-it",
                "task_type": "CAUSAL_LM",
                "r": 16,
                "lora_alpha": 32,
                "target_modules": ["q_proj", "v_proj"],
                "modules_to_save": ["embed_tokens"]
            }"#,
        )
        .expect("adapter config");

        let args = ModelsLoraInspectArgs {
            base_model: "local-gemma4-e4b".to_string(),
            adapter: adapter_dir.display().to_string(),
            name: Some("burin-tools".to_string()),
            provider: Some("vllm".to_string()),
            manifest: None,
            require_contract_id: false,
            json: true,
        };
        let report = inspect_report(&args).expect("report");
        assert!(report.ok, "{:?}", report.warnings);
        assert_eq!(report.adapter.peft_type.as_deref(), Some("LORA"));
        assert_eq!(report.adapter.rank, Some(16));
        assert_eq!(
            report.adapter.modules_to_save,
            vec!["embed_tokens".to_string()]
        );
        assert_eq!(report.base.tool_format, "json");
        assert!(!report.tool_calling.native_tools);
        assert_eq!(
            report.compatibility.base_model_match,
            BaseModelMatch::Suffix
        );
        assert!(report.compatibility.provider_supports_lora_launch);
        assert!(report.compatibility.provider_supports_lora_max_rank);
        assert_eq!(report.launch.request_model, "burin-tools");
        assert_eq!(report.launch.max_lora_rank, Some(16));
        assert!(report
            .launch
            .harn_local_launch
            .iter()
            .any(|arg| arg == "--lora-adapter"));
        assert!(report
            .launch
            .harn_local_launch
            .windows(2)
            .any(|pair| pair == ["--max-lora-rank", "16"]));
    }

    #[test]
    fn inspect_omits_launch_argv_when_provider_lacks_lora_flags() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let adapter_dir = tmp.path().join("burin-tools");
        std::fs::create_dir(&adapter_dir).expect("adapter dir");
        std::fs::write(adapter_dir.join("adapter_model.safetensors"), b"stub")
            .expect("adapter weights");
        std::fs::write(
            adapter_dir.join("adapter_config.json"),
            r#"{
                "peft_type": "LORA",
                "base_model_name_or_path": "google/gemma-4-e4b-it",
                "r": 16
            }"#,
        )
        .expect("adapter config");

        let args = ModelsLoraInspectArgs {
            base_model: "local-gemma4-e4b".to_string(),
            adapter: adapter_dir.display().to_string(),
            name: Some("burin-tools".to_string()),
            provider: Some("openai".to_string()),
            manifest: None,
            require_contract_id: false,
            json: true,
        };
        let report = inspect_report(&args).expect("report");
        assert!(report.ok, "{:?}", report.warnings);
        assert!(!report.compatibility.provider_supports_lora_launch);
        assert!(!report.compatibility.provider_supports_lora_max_rank);
        assert_eq!(report.launch.request_model, "burin-tools");
        assert_eq!(report.launch.max_lora_rank, None);
        assert!(report.launch.harn_local_launch.is_empty());
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("provider openai")));
    }

    #[test]
    fn inspect_canonicalizes_local_vllm_provider_alias() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let adapter_dir = tmp.path().join("burin-tools");
        std::fs::create_dir(&adapter_dir).expect("adapter dir");
        std::fs::write(adapter_dir.join("adapter_model.safetensors"), b"stub")
            .expect("adapter weights");
        std::fs::write(
            adapter_dir.join("adapter_config.json"),
            r#"{
                "peft_type": "LORA",
                "base_model_name_or_path": "google/gemma-4-e4b-it",
                "r": 16
            }"#,
        )
        .expect("adapter config");

        let args = ModelsLoraInspectArgs {
            base_model: "local-gemma4-e4b".to_string(),
            adapter: adapter_dir.display().to_string(),
            name: Some("burin-tools".to_string()),
            provider: Some("local-vllm".to_string()),
            manifest: None,
            require_contract_id: false,
            json: true,
        };
        let report = inspect_report(&args).expect("report");
        assert_eq!(report.base.provider, "vllm");
        assert_eq!(report.serving.provider, "vllm");
        assert!(report.compatibility.provider_supports_lora_launch);
        assert!(report.compatibility.provider_supports_lora_max_rank);
        assert!(report
            .launch
            .harn_local_launch
            .windows(2)
            .any(|pair| pair == ["--provider", "vllm"]));
    }

    #[test]
    fn mismatched_base_model_marks_report_failed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let adapter_dir = tmp.path().join("other");
        std::fs::create_dir(&adapter_dir).expect("adapter dir");
        std::fs::write(adapter_dir.join("adapter_model.safetensors"), b"stub")
            .expect("adapter weights");
        std::fs::write(
            adapter_dir.join("adapter_config.json"),
            r#"{"peft_type":"LORA","base_model_name_or_path":"other/model"}"#,
        )
        .expect("adapter config");

        let args = ModelsLoraInspectArgs {
            base_model: "local-gemma4-e4b".to_string(),
            adapter: adapter_dir.display().to_string(),
            name: None,
            provider: Some("vllm".to_string()),
            manifest: None,
            require_contract_id: false,
            json: true,
        };
        let report = inspect_report(&args).expect("report");
        assert!(!report.ok);
        assert_eq!(
            report.compatibility.base_model_match,
            BaseModelMatch::Mismatch
        );
    }

    #[test]
    fn lora_plan_template_selection_keeps_native_gemma4_distinct_from_harn_text() {
        let native = template_recipe_for_route("google/gemma-4-E4B-it", "gemma4", "", "native");
        assert_eq!(native.name, "gemma4_native_function_calling");
        assert!(native
            .requirements
            .iter()
            .any(|item| item.contains("messages plus tools JSON schemas")));

        let json = template_recipe_for_route("google/gemma-4-E4B-it", "gemma4", "", "json");
        assert_eq!(json.name, "harn_text_tool_calls_json_fences");
        assert!(json
            .requirements
            .iter()
            .any(|item| item.contains("Harn before training")));
    }

    #[test]
    fn lora_trainer_contract_keeps_loss_masks_and_tool_columns_explicit() {
        let native = trainer_contract_for_dataset(
            "messages_with_tool_calls",
            "native",
            "trl_sft_trainer",
            &[],
        );
        assert!(native
            .iter()
            .any(|item| item.contains("assistant_only_loss=true")));
        assert!(native
            .iter()
            .any(|item| item.contains("messages plus a tools column")));
        assert!(native.iter().any(|item| item.contains("generation masks")));

        let text = trainer_contract_for_dataset(
            "harn_text_tool_calls_json_fences",
            "json",
            "trl_sft_trainer",
            &[],
        );
        assert!(text.iter().any(|item| item.contains("assistant_tool_text")));
        assert!(text
            .iter()
            .any(|item| item.contains("Harn remains the parser")));

        let native_contract = lora_training_contract("messages_with_tool_calls", "native", &[]);
        assert_eq!(
            native_contract.schema_version,
            LORA_TRAINING_CONTRACT_SCHEMA_VERSION
        );
        assert_eq!(
            native_contract.assistant_mask_policy,
            "require_chat_template_generation_masks"
        );
        assert_eq!(
            native_contract.tool_parser_owner,
            "provider_tokenizer_runtime"
        );
        assert_eq!(
            native_contract.dataset_split_policy,
            "train_tune_holdout_disjoint_no_eval_holdout_training"
        );

        let text_contract = lora_training_contract("harn_text_tool_calls_json_fences", "json", &[]);
        assert_eq!(
            text_contract.peft_save_policy.schema_version,
            LORA_PEFT_SAVE_POLICY_SCHEMA_VERSION
        );
        assert_eq!(text_contract.tool_parser_owner, "harn_text_tool_parser");
        assert!(text_contract.peft_save_policy.modules_to_save.is_empty());
        assert!(!text_contract.peft_save_policy.requires_weight_tying_check);

        let embedding_contract = lora_training_contract(
            "harn_text_tool_calls_json_fences",
            "json",
            &["embed_tokens".to_string(), "lm_head".to_string()],
        );
        assert_eq!(
            embedding_contract.peft_save_policy.modules_to_save,
            vec!["embed_tokens".to_string(), "lm_head".to_string()]
        );
        assert!(
            embedding_contract
                .peft_save_policy
                .requires_weight_tying_check
        );

        let unsloth = trainer_contract_for_dataset(
            "harn_text_tool_calls_json_fences",
            "json",
            "unsloth_sft",
            &[],
        );
        assert!(unsloth.iter().any(|item| item.contains("Unsloth")));
        assert!(unsloth.iter().any(|item| item.contains("torch/CUDA")));
        assert!(unsloth
            .iter()
            .any(|item| item.contains("modules_to_save=[]")));
    }

    #[test]
    fn lora_plan_normalizes_hyperparameters_for_serving_contract() {
        let default_args = ModelsLoraPlanArgs {
            base_model: "local-gemma4-e4b".to_string(),
            provider: Some("vllm".to_string()),
            tool_format: "json".to_string(),
            corpus: None,
            teacher: None,
            corpus_strategy: "auto".to_string(),
            method: "qlora".to_string(),
            trainer: "trl_sft_trainer".to_string(),
            rank: 24,
            alpha: None,
            dropout: 0.1,
            modules_to_save: Vec::new(),
            json: true,
        };
        let report = plan_report(&default_args).expect("report");
        assert_eq!(report.training.rank, 24);
        assert_eq!(report.training.alpha, 48);
        assert_eq!(report.training.dropout, 0.1);
        assert!(report
            .launch
            .local_launch_command
            .windows(2)
            .any(|pair| pair == ["--max-lora-rank", "24"]));

        let explicit_args = ModelsLoraPlanArgs {
            alpha: Some(32),
            ..default_args
        };
        let explicit = plan_report(&explicit_args).expect("explicit report");
        assert_eq!(explicit.training.alpha, 32);
    }

    #[test]
    fn lora_plan_canonicalizes_local_vllm_provider_alias() {
        let args = ModelsLoraPlanArgs {
            base_model: "local-gemma4-e4b".to_string(),
            provider: Some("local-vllm".to_string()),
            tool_format: "json".to_string(),
            corpus: Some("lora-corpus".to_string()),
            teacher: None,
            corpus_strategy: "auto".to_string(),
            method: "qlora".to_string(),
            trainer: "trl_sft_trainer".to_string(),
            rank: 24,
            alpha: None,
            dropout: 0.1,
            modules_to_save: Vec::new(),
            json: true,
        };
        let report = plan_report(&args).expect("report");
        assert_eq!(report.base.provider, "vllm");
        assert_eq!(report.serving.adapter_binding, "runtime_lora_adapter");
        assert_eq!(
            report.serving.lora_module_value_format,
            "json_with_base_model"
        );
        assert!(report
            .launch
            .local_launch_command
            .windows(2)
            .any(|pair| pair == ["--provider", "vllm"]));
        assert!(report
            .launch
            .local_launch_command
            .windows(2)
            .any(|pair| pair == ["--max-lora-rank", "24"]));
    }

    #[test]
    fn lora_plan_records_model_aware_selection_contract() {
        let args = ModelsLoraPlanArgs {
            base_model: "local-gemma4-e4b".to_string(),
            provider: Some("vllm".to_string()),
            tool_format: "json".to_string(),
            corpus: Some("lora-corpus".to_string()),
            teacher: Some("dashscope/qwen3-coder-next".to_string()),
            corpus_strategy: "refresh".to_string(),
            method: "qlora".to_string(),
            trainer: "unsloth_trl_sft".to_string(),
            rank: 24,
            alpha: None,
            dropout: 0.1,
            modules_to_save: Vec::new(),
            json: true,
        };
        let report = plan_report(&args).expect("report");
        assert_eq!(report.training.trainer, "unsloth_sft");
        let selection = &report.corpus_refresh.model_aware_selection;
        assert!(selection
            .difficulty_signals
            .iter()
            .any(|item| item.contains("target base-model outcome bucket")));
        assert!(selection
            .sampling_policy
            .iter()
            .any(|item| item.contains("medium-difficulty")));
        assert!(selection
            .refinement_loop
            .iter()
            .any(|item| item.contains("parser-valid teacher repairs")));
        assert!(selection
            .stop_conditions
            .iter()
            .any(|item| item.contains("no-write")));
    }

    #[test]
    fn lora_plan_emits_post_training_receipt_and_probe_commands() {
        let args = ModelsLoraPlanArgs {
            base_model: "local-gemma4-e4b".to_string(),
            provider: Some("vllm".to_string()),
            tool_format: "json".to_string(),
            corpus: Some("lora-corpus".to_string()),
            teacher: Some("dashscope/qwen3-coder-next".to_string()),
            corpus_strategy: "refresh".to_string(),
            method: "qlora".to_string(),
            trainer: "unsloth_sft".to_string(),
            rank: 24,
            alpha: Some(48),
            dropout: 0.1,
            modules_to_save: vec!["embed_tokens".to_string(), "lm_head".to_string()],
            json: true,
        };
        let report = plan_report(&args).expect("report");
        assert_eq!(
            report.training.contract.schema_version,
            LORA_TRAINING_CONTRACT_SCHEMA_VERSION
        );
        assert_eq!(
            report.training.contract.peft_save_policy.modules_to_save,
            vec!["embed_tokens".to_string(), "lm_head".to_string()]
        );
        assert!(
            report
                .training
                .contract
                .peft_save_policy
                .requires_weight_tying_check
        );
        assert!(report
            .launch
            .train_command
            .windows(2)
            .any(|pair| pair == ["--modules-to-save", "embed_tokens"]));
        assert!(report
            .launch
            .train_command
            .windows(2)
            .any(|pair| pair == ["--modules-to-save", "lm_head"]));
        assert!(report
            .launch
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--export-manifest", "ADAPTER_DATASET.manifest.json"]));
        assert!(report
            .launch
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--chat-template", "harn_text_tool_calls_json_fences"]));
        assert!(report
            .launch
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--teacher", "dashscope/qwen3-coder-next"]));
        assert!(report
            .launch
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--trainer", "unsloth_sft"]));
        assert!(report
            .launch
            .train_command
            .windows(2)
            .any(|pair| pair == ["--trainer", "unsloth_sft"]));
        assert!(report
            .launch
            .train_command
            .windows(2)
            .any(|pair| pair == ["--receipt-out", "ADAPTER_OUTPUT_DIR/train.receipt.json"]));
        assert!(report
            .launch
            .train_command
            .windows(2)
            .any(|pair| pair == ["--export-manifest", "ADAPTER_DATASET.manifest.json"]));
        assert!(report.launch.manifest_command.windows(2).any(|pair| pair
            == [
                "--target-metadata",
                "serving_base_precision=same_quantization_family_as_training_or_revalidate"
            ]));
        assert_eq!(
            report.launch.tool_probe_command,
            [
                "harn",
                "provider",
                "tool-probe",
                "vllm",
                "--model",
                "ADAPTER_MODEL",
                "--mode",
                "both",
                "--repeat",
                "5",
                "--json",
            ]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
        );
        let evidence = &report.evaluation.evidence_contract;
        assert_eq!(
            evidence.schema_version,
            LORA_PROMOTION_EVIDENCE_SCHEMA_VERSION
        );
        assert!(evidence.promotion_id.starts_with("sha256:"));
        assert_eq!(evidence.base_route.model, report.base.id);
        assert_eq!(evidence.adapter_route.model, "ADAPTER_MODEL");
        assert_eq!(evidence.adapter_route.tool_format, "json");
        assert!(evidence
            .required_receipts
            .iter()
            .any(|receipt| receipt == "lora_adapter_manifest"));
        assert!(evidence
            .required_receipts
            .iter()
            .any(|receipt| receipt == "promotion_probe_matrix_receipt"));
        let probe_case_ids = evidence
            .required_probe_cases
            .iter()
            .map(|probe_case| probe_case.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            probe_case_ids,
            [
                "sequential_tool_call",
                "parallel_tool_calls",
                "no_tool_answer",
                "unavailable_tool_repair",
                "multi_turn_tool_result_continuation",
                "serving_concurrency_probe",
            ]
        );
        assert!(evidence.required_probe_cases.iter().any(|probe_case| {
            probe_case.id == "parallel_tool_calls"
                && probe_case
                    .requirement
                    .contains("required_when_route_supports_parallel_tool_calls")
        }));
        assert!(evidence
            .optional_batch_receipts
            .iter()
            .any(|receipt| receipt == "harn.model_batch_results_receipt"));
        assert_eq!(evidence.batch_ready.workload, "eval");
        assert!(evidence
            .batch_ready
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--id-prefix", "lora-promotion"]));
    }

    #[test]
    fn lora_promotion_id_tracks_acceptance_gate_drift() {
        let metrics = vec!["exact tool-name + argument match rate".to_string()];
        let original_gates = vec!["require a positive paired lift".to_string()];
        let tightened_gates = vec![
            "require a positive paired lift".to_string(),
            "require no non-tool smoke regression".to_string(),
        ];
        let original = PromotionEvidenceInput {
            contract_id: "sha256:contract",
            base_model: "base",
            provider: "vllm",
            request_model: "adapter",
            tool_format: "json",
            eval_dataset: "tool-calls",
            minimum_trials: 5,
            required_metrics: &metrics,
            gates: &original_gates,
        };
        let required_probe_cases = lora_required_probe_cases(original.tool_format);
        let original_id = lora_promotion_id(&original, &required_probe_cases);
        let tightened = PromotionEvidenceInput {
            gates: &tightened_gates,
            ..original
        };

        assert_ne!(
            original_id,
            lora_promotion_id(&tightened, &required_probe_cases)
        );
    }

    #[test]
    fn lora_promotion_id_tracks_probe_matrix_drift() {
        let metrics = vec!["exact tool-name + argument match rate".to_string()];
        let gates = vec!["require a positive paired lift".to_string()];
        let input = PromotionEvidenceInput {
            contract_id: "sha256:contract",
            base_model: "base",
            provider: "vllm",
            request_model: "adapter",
            tool_format: "json",
            eval_dataset: "tool-calls",
            minimum_trials: 5,
            required_metrics: &metrics,
            gates: &gates,
        };
        let required_probe_cases = lora_required_probe_cases(input.tool_format);
        let mut changed_probe_cases = required_probe_cases.clone();
        changed_probe_cases[0]
            .expected
            .push_str(" with stable ordering");

        assert_ne!(
            lora_promotion_id(&input, &required_probe_cases),
            lora_promotion_id(&input, &changed_probe_cases)
        );
    }

    #[test]
    fn lora_serving_recipe_keeps_runtime_binding_explicit() {
        let has_requirement = |recipe: &ServingRecipe,
                               kind: &str,
                               name: &str,
                               value: Option<&str>,
                               required: bool| {
            recipe.serving_requirements.iter().any(|requirement| {
                requirement.kind == kind
                    && requirement.name == name
                    && requirement.value.as_deref() == value
                    && requirement.required == required
            })
        };

        let supported = serving_recipe(
            "gemma-4-e4b-it",
            "vllm",
            "ADAPTER_MODEL",
            "ADAPTER_NAME",
            "json",
            "harn_text_tool_calls_json_fences",
            true,
            "json_with_base_model",
        );
        assert_eq!(supported.adapter_binding, "runtime_lora_adapter");
        assert_eq!(supported.lora_module_value_format, "json_with_base_model");
        assert!(supported
            .runtime_notes
            .iter()
            .any(|note| note.contains("per request model name")));
        assert!(supported
            .runtime_notes
            .iter()
            .any(|note| note.contains("Harn owns tool-call parsing")));
        assert!(has_requirement(
            &supported,
            "parser_owner",
            "tool_call_parser",
            Some("harn_text_tool_parser"),
            true,
        ));
        assert!(has_requirement(
            &supported,
            "provider_native_tool_parser",
            "native_tool_parser_mode",
            Some("disabled_unless_proxy_maps_to_harn_text"),
            true,
        ));

        let external = serving_recipe(
            "gemma-4-e4b-it",
            "external",
            "ADAPTER_MODEL",
            "ADAPTER_NAME",
            "json",
            "harn_text_tool_calls_json_fences",
            false,
            "name_path",
        );
        assert_eq!(
            external.adapter_binding,
            "external_runtime_or_merged_adapter"
        );
        assert!(external
            .runtime_notes
            .iter()
            .any(|note| note.contains("external runtime")));

        let native_functiongemma = serving_recipe(
            "google/functiongemma-270m-it",
            "vllm",
            "ADAPTER_MODEL",
            "ADAPTER_NAME",
            "native",
            "messages_with_tool_calls",
            true,
            "json_with_base_model",
        );
        assert!(native_functiongemma
            .runtime_notes
            .iter()
            .any(|note| note.contains("--enable-auto-tool-choice")));
        assert!(native_functiongemma
            .runtime_notes
            .iter()
            .any(|note| note.contains("functiongemma parser/chat template")));
        assert!(has_requirement(
            &native_functiongemma,
            "server_flag",
            "--tool-call-parser",
            Some("functiongemma"),
            true,
        ));
        assert!(has_requirement(
            &native_functiongemma,
            "chat_template",
            "chat_template",
            Some("functiongemma_control_tokens"),
            true,
        ));

        let native_gemma4 = serving_recipe(
            "google/gemma-4-e4b-it",
            "vllm",
            "ADAPTER_MODEL",
            "ADAPTER_NAME",
            "native",
            "messages_with_tool_calls",
            true,
            "json_with_base_model",
        );
        assert!(has_requirement(
            &native_gemma4,
            "server_flag",
            "--tool-call-parser",
            Some("gemma4"),
            true,
        ));
        assert!(has_requirement(
            &native_gemma4,
            "server_flag",
            "--reasoning-parser",
            Some("gemma4"),
            false,
        ));
        assert!(has_requirement(
            &native_gemma4,
            "chat_template",
            "chat_template",
            Some("examples/tool_chat_template_gemma4.jinja"),
            true,
        ));
        assert!(has_requirement(
            &native_gemma4,
            "manifest_metadata",
            "chat_template_hash",
            None,
            true,
        ));
    }
}
