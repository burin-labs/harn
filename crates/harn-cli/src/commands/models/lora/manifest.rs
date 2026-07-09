use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::ModelsLoraManifestArgs;

use super::{
    adapter_name_from_input, dataset_format_for_tool_format, lora_contract_id,
    lora_contract_report, lora_evaluation_recipe, lora_modules_value_format,
    lora_training_contract, merge_serving_target_metadata, normalize_lora_alpha,
    normalize_lora_dropout, normalize_lora_method, normalize_lora_rank, normalize_lora_trainer,
    normalize_modules_to_save, normalize_plan_tool_format, normalize_tool_catalog_policy,
    parse_target_metadata, render_embedded_lora_report, resolve_lora_provider, serving_recipe,
    sha256_file, target_modules_for_route, teacher_report, template_recipe_for_route,
    tool_catalog_contract, BaseModelReport, EvaluationRecipe, LoraContractReport,
    LoraTrainingContract, PrecisionContract, ServingRecipe, ServingRecipeInput, TeacherReport,
    TemplateRecipe, ToolCallingReport,
};

const LORA_MANIFEST_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_MANIFEST_PAYLOAD_JSON";
const LORA_MANIFEST_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_MANIFEST_PAYLOAD_PRETTY";

pub(super) async fn manifest(args: &ModelsLoraManifestArgs) -> i32 {
    let report = match manifest_report(args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    if let Some(path) = args.out.as_deref() {
        if let Err(error) = write_manifest(path, &report) {
            eprintln!("error: {error}");
            return 1;
        }
    }
    render_embedded_lora_report(
        &report,
        LORA_MANIFEST_PAYLOAD_ENV,
        LORA_MANIFEST_PAYLOAD_PRETTY_ENV,
        "models/lora_manifest",
        args.json,
        "LoRA manifest",
    )
    .await
}

fn manifest_report(args: &ModelsLoraManifestArgs) -> Result<LoraManifestReport, String> {
    let method = normalize_lora_method(&args.method)?;
    let trainer = normalize_lora_trainer(&args.trainer)?;
    let rank = normalize_lora_rank(args.rank)?;
    let alpha = normalize_lora_alpha(args.alpha, rank)?;
    let dropout = normalize_lora_dropout(args.dropout)?;
    let requested_tool_format = normalize_plan_tool_format(&args.tool_format)?;
    let modules_to_save = normalize_modules_to_save(&args.modules_to_save)?;
    let tool_catalog_policy = normalize_tool_catalog_policy(&args.tool_catalog_policy)?;
    let tool_catalog = tool_catalog_contract(
        &tool_catalog_policy,
        args.tool_catalog_id.as_deref(),
        args.tool_catalog_hash.as_deref(),
    )?;
    let resolved = harn_vm::llm_config::resolve_model_info(&args.base_model);
    let provider = resolve_lora_provider(args.provider.as_deref(), &resolved.provider);
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
    let template = template_recipe_for_route(
        &resolved.id,
        &resolved.family,
        &resolved.lineage,
        &decision.effective,
    );
    let chat_template = args
        .chat_template
        .clone()
        .unwrap_or_else(|| template.name.clone());
    let adapter_name = args
        .adapter_name
        .clone()
        .or_else(|| args.request_model.clone())
        .or_else(|| {
            args.adapter_path
                .as_deref()
                .map(adapter_name_from_input)
                .filter(|name| !name.is_empty())
        })
        .unwrap_or_else(|| "ADAPTER_NAME".to_string());
    let request_model = args
        .request_model
        .clone()
        .unwrap_or_else(|| adapter_name.clone());
    let contract_id = lora_contract_id(
        &resolved.id,
        &provider,
        &decision.effective,
        dataset_format,
        Some(&chat_template),
        &modules_to_save,
        &tool_catalog,
    )?;
    let contract = lora_contract_report(
        contract_id.clone(),
        &resolved.id,
        &provider,
        &decision.effective,
        dataset_format,
        Some(chat_template.clone()),
        &modules_to_save,
        &tool_catalog,
    );
    let local_runtime =
        harn_vm::llm_config::provider_config(&provider).and_then(|provider| provider.local_runtime);
    let provider_supports_lora_launch = local_runtime
        .as_ref()
        .and_then(|runtime| runtime.lora_modules_arg.as_ref())
        .is_some();
    let lora_module_value_format = lora_modules_value_format(local_runtime.as_ref());
    let serving = serving_recipe(ServingRecipeInput {
        base_model: &resolved.id,
        provider: &provider,
        request_model: &request_model,
        adapter_name: &adapter_name,
        tool_format: &decision.effective,
        dataset_format,
        provider_supports_lora_launch,
        lora_module_value_format: &lora_module_value_format,
        tool_catalog: &tool_catalog,
    });
    let eval_dataset = args
        .dataset
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "TOOL_CALL_EVAL_DATASET".to_string());
    let promotion = lora_evaluation_recipe(
        &contract_id,
        &resolved.id,
        &provider,
        &request_model,
        &decision.effective,
        &eval_dataset,
        vec![
            "harn".to_string(),
            "eval".to_string(),
            "tool-calls".to_string(),
            "--planner".to_string(),
            request_model.clone(),
            "--tool-format".to_string(),
            decision.effective.clone(),
            "--dataset".to_string(),
            eval_dataset.clone(),
        ],
    );
    let teacher = args
        .teacher
        .as_ref()
        .map(|selector| teacher_report(selector));
    let precision = super::precision_contract_for_method(&method);
    let target_modules =
        target_modules_for_route(&method, &resolved.id, &resolved.family, &resolved.lineage);
    let mut warnings = Vec::new();
    let mut metadata = parse_target_metadata(&args.target_metadata)?;
    merge_serving_target_metadata(&mut metadata, &serving, &mut warnings);
    if let Some(correction) = &decision.correction {
        warnings.push(correction.clone());
    }
    warn_missing_path(&mut warnings, "dataset", args.dataset.as_deref());
    warn_missing_path(&mut warnings, "corpus", args.corpus.as_deref());
    warn_missing_path(
        &mut warnings,
        "export manifest",
        args.export_manifest.as_deref(),
    );
    if args.adapter_path.is_none() {
        warnings.push(
            "no --adapter-path supplied; manifest records the route contract before adapter artifact inspection"
                .to_string(),
        );
    } else if let Some(raw) = args.adapter_path.as_deref() {
        let expanded = expand_adapter_path(raw);
        if adapter_reference_is_local_path(raw) && !expanded.exists() {
            warnings.push(format!(
                "adapter path does not exist locally: {}",
                expanded.display()
            ));
        }
    }
    if args.out.is_none() {
        warnings.push("no --out supplied; manifest report was not written to disk".to_string());
    }
    Ok(LoraManifestReport {
        schema_version: 1,
        producer: "harn_models_lora_manifest_v1".to_string(),
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
        request: ManifestRequest {
            requested_tool_format,
            effective_tool_format: decision.effective.clone(),
            tool_format_correction: decision.correction,
            dataset_format: dataset_format.to_string(),
            tool_catalog_policy: tool_catalog.policy.clone(),
            tool_catalog_id: tool_catalog.catalog_id.clone(),
            tool_catalog_hash: tool_catalog.catalog_hash.clone(),
            output: args.out.as_ref().map(|path| path.display().to_string()),
        },
        tool_calling: ToolCallingReport {
            native_tools: capabilities.native_tools,
            preferred_tool_format: capabilities.preferred_tool_format,
            text_tool_wire_format_supported: capabilities.text_tool_wire_format_supported,
            structured_output_mode: capabilities.structured_output_mode,
            recommended_endpoint: capabilities.recommended_endpoint,
        },
        target: ManifestTarget {
            base_model: resolved.id,
            provider,
            adapter_name,
            request_model,
            harn_tool_format: decision.effective.clone(),
            dataset_format: dataset_format.to_string(),
            chat_template,
            contract_id,
            metadata,
        },
        contract,
        training: ManifestTraining {
            run_id: args.training_run_id.clone(),
            trainer,
            trainer_version: args.trainer_version.clone(),
            method,
            adapter_type: "peft_lora".to_string(),
            rank,
            alpha,
            dropout,
            target_modules,
            precision,
            template,
            contract: lora_training_contract(
                dataset_format,
                &decision.effective,
                &modules_to_save,
                &tool_catalog,
            ),
        },
        inputs: ManifestInputs {
            dataset: args.dataset.as_deref().map(path_ref).transpose()?,
            corpus: args.corpus.as_deref().map(path_ref).transpose()?,
            export_manifest: args.export_manifest.as_deref().map(path_ref).transpose()?,
            teacher,
        },
        artifacts: manifest_artifacts(args.adapter_path.as_deref()),
        serving,
        promotion,
        warnings,
    })
}

fn write_manifest(path: &Path, report: &LoraManifestReport) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(report)
            .map_err(|error| format!("failed to render LoRA manifest JSON: {error}"))?
            + "\n",
    )
    .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn warn_missing_path(warnings: &mut Vec<String>, label: &str, path: Option<&Path>) {
    if let Some(path) = path {
        if !path.exists() {
            warnings.push(format!("{label} path does not exist: {}", path.display()));
        }
    }
}

fn manifest_artifacts(adapter_path: Option<&str>) -> ManifestArtifacts {
    let adapter_reference = adapter_path.map(str::to_string);
    let local_path = adapter_path
        .map(expand_adapter_path)
        .filter(|path| path.exists())
        .and_then(|path| path_ref(&path).ok());
    let adapter_files = local_path
        .as_ref()
        .filter(|reference| reference.kind == "directory")
        .map(|reference| adapter_file_refs(Path::new(&reference.path)))
        .unwrap_or_default();
    ManifestArtifacts {
        adapter_reference,
        local_path,
        adapter_files,
    }
}

fn expand_adapter_path(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

fn adapter_reference_is_local_path(raw: &str) -> bool {
    raw.starts_with("./")
        || raw.starts_with("../")
        || raw.starts_with("~/")
        || Path::new(raw).is_absolute()
}

fn adapter_file_refs(dir: &Path) -> Vec<PathRef> {
    [
        "adapter_config.json",
        "adapter_model.safetensors",
        "adapter_model.bin",
    ]
    .into_iter()
    .filter_map(|name| {
        let path = dir.join(name);
        path.exists().then(|| path_ref(&path).ok()).flatten()
    })
    .collect()
}

fn path_ref(path: &Path) -> Result<PathRef, String> {
    let kind = if path.is_file() {
        "file"
    } else if path.is_dir() {
        "directory"
    } else if path.exists() {
        "other"
    } else {
        "missing"
    };
    let sha256 = if path.is_file() {
        Some(sha256_file(path)?)
    } else {
        None
    };
    Ok(PathRef {
        path: path.display().to_string(),
        exists: path.exists(),
        kind: kind.to_string(),
        sha256,
    })
}

#[derive(Debug, Serialize)]
struct LoraManifestReport {
    schema_version: u64,
    producer: String,
    ok: bool,
    base: BaseModelReport,
    request: ManifestRequest,
    tool_calling: ToolCallingReport,
    target: ManifestTarget,
    contract: LoraContractReport,
    training: ManifestTraining,
    inputs: ManifestInputs,
    artifacts: ManifestArtifacts,
    serving: ServingRecipe,
    promotion: EvaluationRecipe,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ManifestRequest {
    requested_tool_format: String,
    effective_tool_format: String,
    tool_format_correction: Option<String>,
    dataset_format: String,
    tool_catalog_policy: String,
    tool_catalog_id: Option<String>,
    tool_catalog_hash: Option<String>,
    output: Option<String>,
}

#[derive(Debug, Serialize)]
struct ManifestTarget {
    base_model: String,
    provider: String,
    adapter_name: String,
    request_model: String,
    harn_tool_format: String,
    dataset_format: String,
    chat_template: String,
    contract_id: String,
    metadata: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct ManifestTraining {
    run_id: Option<String>,
    trainer: String,
    trainer_version: Option<String>,
    method: String,
    adapter_type: String,
    rank: u32,
    alpha: u32,
    dropout: f64,
    target_modules: Vec<String>,
    precision: PrecisionContract,
    template: TemplateRecipe,
    contract: LoraTrainingContract,
}

#[derive(Debug, Serialize)]
struct ManifestInputs {
    dataset: Option<PathRef>,
    corpus: Option<PathRef>,
    export_manifest: Option<PathRef>,
    teacher: Option<TeacherReport>,
}

#[derive(Debug, Serialize)]
struct ManifestArtifacts {
    adapter_reference: Option<String>,
    local_path: Option<PathRef>,
    adapter_files: Vec<PathRef>,
}

#[derive(Debug, Serialize)]
struct PathRef {
    path: String,
    exists: bool,
    kind: String,
    sha256: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_args_with_trainer(trainer: &str) -> ModelsLoraManifestArgs {
        ModelsLoraManifestArgs {
            base_model: "local-gemma4-e4b".to_string(),
            provider: Some("local-vllm".to_string()),
            tool_format: "json".to_string(),
            dataset: None,
            corpus: None,
            export_manifest: None,
            out: None,
            adapter_name: Some("burin-tools".to_string()),
            adapter_path: None,
            request_model: None,
            chat_template: None,
            trainer: trainer.to_string(),
            trainer_version: Some("trainer-2026.7".to_string()),
            method: "qlora".to_string(),
            rank: 16,
            alpha: None,
            dropout: 0.05,
            training_run_id: Some("train-001".to_string()),
            teacher: None,
            target_metadata: vec!["lane=tool-calls".to_string()],
            tool_catalog_policy: "full_schema".to_string(),
            tool_catalog_id: None,
            tool_catalog_hash: None,
            modules_to_save: Vec::new(),
            json: true,
        }
    }

    #[test]
    fn manifest_report_normalizes_trainer_aliases() {
        let mlx_report = manifest_report(&manifest_args_with_trainer("mlx-lm")).expect("mlx");
        assert_eq!(mlx_report.training.trainer, "mlx_lm");
        assert_eq!(
            mlx_report.training.trainer_version.as_deref(),
            Some("trainer-2026.7")
        );

        let unsloth_report =
            manifest_report(&manifest_args_with_trainer("unsloth_trl_sft")).expect("unsloth");
        assert_eq!(unsloth_report.training.trainer, "unsloth_sft");
    }

    #[test]
    fn manifest_report_rejects_unknown_trainer() {
        let error = manifest_report(&manifest_args_with_trainer("homegrown_python"))
            .expect_err("unknown trainer should fail closed");
        assert!(error.contains("unsupported LoRA trainer"));
    }
}
