use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::cli::{ModelsLoraArgs, ModelsLoraCommand, ModelsLoraInspectArgs, ModelsLoraPlanArgs};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

mod export;

const LORA_INSPECT_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_INSPECT_PAYLOAD_JSON";
const LORA_INSPECT_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_INSPECT_PAYLOAD_PRETTY";
const LORA_PLAN_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_PLAN_PAYLOAD_JSON";
const LORA_PLAN_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_PLAN_PAYLOAD_PRETTY";
/// Serialises the dispatch path so concurrent in-process callers do not race on
/// the env vars that carry the Rust-collected adapter/catalog facts.
static DISPATCH_LORA_INSPECT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn run(args: ModelsLoraArgs) {
    let exit_code = match args.command {
        ModelsLoraCommand::Export(args) => export::export_dataset(&args).await,
        ModelsLoraCommand::Inspect(args) => inspect(&args).await,
        ModelsLoraCommand::Plan(args) => plan(&args).await,
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
    let payload_json = match serde_json::to_string(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise LoRA inspect payload: {error}");
            return 1;
        }
    };
    let pretty_json = match serde_json::to_string_pretty(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to render LoRA inspect JSON: {error}");
            return 1;
        }
    };

    let _guard = DISPATCH_LORA_INSPECT_LOCK.lock().await;
    let _payload = ScopedEnvVar::set(LORA_INSPECT_PAYLOAD_ENV, &payload_json);
    let _pretty = ScopedEnvVar::set(LORA_INSPECT_PAYLOAD_PRETTY_ENV, &pretty_json);
    let outcome = dispatch::run_embedded_script("models/lora_inspect", Vec::new(), args.json).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}

async fn plan(args: &ModelsLoraPlanArgs) -> i32 {
    let report = match plan_report(args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let payload_json = match serde_json::to_string(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise LoRA plan payload: {error}");
            return 1;
        }
    };
    let pretty_json = match serde_json::to_string_pretty(&report) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to render LoRA plan JSON: {error}");
            return 1;
        }
    };

    let _guard = DISPATCH_LORA_INSPECT_LOCK.lock().await;
    let _payload = ScopedEnvVar::set(LORA_PLAN_PAYLOAD_ENV, &payload_json);
    let _pretty = ScopedEnvVar::set(LORA_PLAN_PAYLOAD_PRETTY_ENV, &pretty_json);
    let outcome = dispatch::run_embedded_script("models/lora_plan", Vec::new(), args.json).await;
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
        .map(str::to_string)
        .unwrap_or_else(|| resolved.provider.clone());
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
        require_adapter_contract_id,
        manifest: InspectContractManifest {
            base_model: manifest_base_model,
            provider: manifest_provider,
            harn_tool_format: manifest_tool_format,
            dataset_format: manifest_dataset_format,
            chat_template: manifest_chat_template,
            adapter_name: target_adapter_name,
            request_model: serving_request_model,
        },
        warnings,
    }))
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
    let rank = normalize_lora_rank(args.rank)?;
    let alpha = normalize_lora_alpha(args.alpha, rank)?;
    let dropout = normalize_lora_dropout(args.dropout)?;
    let quantization = quantization_for_method(&method).to_string();
    let requested_tool_format = normalize_plan_tool_format(&args.tool_format)?;
    let requested_corpus_strategy = normalize_corpus_strategy(&args.corpus_strategy)?;
    let resolved = harn_vm::llm_config::resolve_model_info(&args.base_model);
    let provider = args
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| resolved.provider.clone());
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
    let template = template_recipe_for_route(
        &resolved.id,
        &resolved.family,
        &resolved.lineage,
        &decision.effective,
    );
    let export_corpus_arg = corpus
        .clone()
        .unwrap_or_else(|| "CORPUS_JSONL_OR_DIR".to_string());
    let export_command = vec![
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
        export_corpus_arg,
        "--out".to_string(),
        "ADAPTER_DATASET.jsonl".to_string(),
        "--manifest".to_string(),
        "ADAPTER_DATASET.manifest.json".to_string(),
        "--adapter-name".to_string(),
        adapter_name.clone(),
        "--chat-template".to_string(),
        template.name.clone(),
    ];
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
    let warnings = plan_warnings(
        &provider,
        &decision,
        provider_supports_lora_launch,
        capabilities.native_tools,
        &requested_tool_format,
        &requested_corpus_strategy,
        &effective_corpus_strategy,
        teacher.as_ref(),
    );
    Ok(LoraPlanReport {
        ok: true,
        base: BaseModelReport {
            selector: args.base_model.clone(),
            id: resolved.id.clone(),
            provider,
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
            trainer: "trl_sft_trainer".to_string(),
            rank,
            alpha,
            dropout,
            quantization,
            loss_scope: "assistant_tool_calls".to_string(),
            packing: "off_by_default_for_tool_boundaries".to_string(),
            target_modules: vec![
                "q_proj".to_string(),
                "k_proj".to_string(),
                "v_proj".to_string(),
                "o_proj".to_string(),
            ],
            contract: lora_training_contract(dataset_format, &decision.effective),
            trainer_contract: trainer_contract_for_dataset(dataset_format, &decision.effective),
            notes: training_notes(&decision.effective),
        },
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
        evaluation: lora_evaluation_recipe(&decision.effective, eval_command),
        serving,
        launch: PlanLaunchHints {
            export_command,
            inspect_command,
            local_launch_command: launch_command,
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

fn trainer_contract_for_dataset(dataset_format: &str, tool_format: &str) -> Vec<String> {
    let machine_contract = lora_training_contract(dataset_format, tool_format);
    let mut contract = vec![
        "use TRL SFTTrainer with PEFT LoRA/QLoRA; keep the base weights frozen and save only adapter artifacts".to_string(),
        "set assistant_only_loss=true so prompts, tool schemas, and tool observations are context rather than targets".to_string(),
        "verify the tokenizer chat template emits assistant generation masks before trusting assistant_only_loss".to_string(),
        "keep packing=false unless a boundary-aware packer preserves complete tool-call/tool-result pairs".to_string(),
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
) -> LoraTrainingContract {
    LoraTrainingContract {
        schema_version: 1,
        loss_scope: "assistant_tool_calls".to_string(),
        assistant_mask_policy: "require_chat_template_generation_masks".to_string(),
        packing_policy: "disabled_unless_boundary_aware_tool_pack_pairs".to_string(),
        tool_parser_owner: tool_parser_owner_for_format(tool_format).to_string(),
        dataset_format: dataset_format.to_string(),
        dataset_split_policy: "train_tune_holdout_disjoint_no_eval_holdout_training".to_string(),
        required_example_metadata: vec![
            "dataset_format".to_string(),
            "source_tool_format".to_string(),
            "lora_contract_id".to_string(),
            "lora_target".to_string(),
        ],
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
    ServingRecipe {
        request_model: request_model.to_string(),
        adapter_name: adapter_name.to_string(),
        base_model: base_model.to_string(),
        provider: provider.to_string(),
        adapter_binding,
        lora_module_value_format: lora_module_value_format.to_string(),
        tool_format: tool_format.to_string(),
        dataset_format: dataset_format.to_string(),
        runtime_notes,
        promotion_gates: vec![
            "inspect the adapter against the exact served base model before launch".to_string(),
            "run base-versus-adapter tool-call evals with the same request model selector"
                .to_string(),
            "keep a rollback path to the base route or previous adapter revision".to_string(),
        ],
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
    }
}

pub(super) fn lora_evaluation_recipe(
    tool_format: &str,
    eval_command: Vec<String>,
) -> EvaluationRecipe {
    let parser_metric = if matches!(tool_format, "text" | "json") {
        "Harn text parser acceptance rate"
    } else {
        "native tool-call schema acceptance rate"
    };
    EvaluationRecipe {
        holdout_policy: "keep train/tune/holdout splits disjoint; never train on Harn eval fixtures"
            .to_string(),
        minimum_trials: 5,
        comparison_baseline: "same base model, provider, tool format, prompt template, and tool schemas without the adapter"
            .to_string(),
        required_metrics: vec![
            "exact tool-name + argument match rate".to_string(),
            parser_metric.to_string(),
            "malformed-call and prose-only failure rate".to_string(),
            "wrong-tool false positive rate".to_string(),
            "latency and cost per solved tool-call case".to_string(),
        ],
        gates: vec![
            "compare base versus adapter on identical tool-call cases".to_string(),
            "require a positive paired lift before promotion; inconclusive movement stays experimental"
                .to_string(),
            "require zero contract-id drift between export manifest, adapter metadata, and served route"
                .to_string(),
            "require no regression on non-tool chat smoke prompts".to_string(),
        ],
        eval_command,
    }
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
struct LoraPlanReport {
    ok: bool,
    base: BaseModelReport,
    request: PlanRequest,
    tool_calling: ToolCallingReport,
    training: TrainingRecipe,
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
pub(super) struct LoraTrainingContract {
    schema_version: u64,
    loss_scope: String,
    assistant_mask_policy: String,
    packing_policy: String,
    tool_parser_owner: String,
    dataset_format: String,
    dataset_split_policy: String,
    required_example_metadata: Vec<String>,
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
}

#[derive(Debug, Serialize)]
pub(super) struct EvaluationRecipe {
    holdout_policy: String,
    minimum_trials: u64,
    comparison_baseline: String,
    required_metrics: Vec<String>,
    gates: Vec<String>,
    eval_command: Vec<String>,
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
    runtime_notes: Vec<String>,
    promotion_gates: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PlanLaunchHints {
    export_command: Vec<String>,
    inspect_command: Vec<String>,
    local_launch_command: Vec<String>,
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
                "target_modules": ["q_proj", "v_proj"]
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
        let native = trainer_contract_for_dataset("messages_with_tool_calls", "native");
        assert!(native
            .iter()
            .any(|item| item.contains("assistant_only_loss=true")));
        assert!(native
            .iter()
            .any(|item| item.contains("messages plus a tools column")));
        assert!(native.iter().any(|item| item.contains("generation masks")));

        let text = trainer_contract_for_dataset("harn_text_tool_calls_json_fences", "json");
        assert!(text.iter().any(|item| item.contains("assistant_tool_text")));
        assert!(text
            .iter()
            .any(|item| item.contains("Harn remains the parser")));

        let native_contract = lora_training_contract("messages_with_tool_calls", "native");
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

        let text_contract = lora_training_contract("harn_text_tool_calls_json_fences", "json");
        assert_eq!(text_contract.tool_parser_owner, "harn_text_tool_parser");
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
            rank: 24,
            alpha: None,
            dropout: 0.1,
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
    fn lora_serving_recipe_keeps_runtime_binding_explicit() {
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
    }
}
