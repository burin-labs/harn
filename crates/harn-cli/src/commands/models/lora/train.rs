use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;
use serde_json::Value;

use crate::cli::ModelsLoraTrainArgs;

use super::{
    adapter_name_from_input, dataset_format_for_tool_format, lora_contract_id,
    lora_contract_report, lora_evaluation_recipe, lora_modules_value_format,
    lora_training_contract, merge_serving_target_metadata, normalize_lora_alpha,
    normalize_lora_dropout, normalize_lora_method, normalize_lora_rank, normalize_lora_trainer,
    normalize_modules_to_save, normalize_plan_tool_format, parse_target_metadata,
    precision_contract_for_method, render_embedded_lora_report, resolve_lora_provider,
    serving_recipe, sha256_file, target_modules_for_route, teacher_report,
    template_recipe_for_route, trainer_contract_for_dataset, BaseModelReport, EvaluationRecipe,
    LoraContractReport, LoraTrainingContract, PrecisionContract, ServingRecipe, TeacherReport,
    TemplateRecipe, ToolCallingReport,
};

const LORA_TRAIN_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_TRAIN_PAYLOAD_JSON";
const LORA_TRAIN_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_TRAIN_PAYLOAD_PRETTY";

pub(super) async fn train(args: &ModelsLoraTrainArgs) -> i32 {
    let mut report = match train_report(args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    if args.execute {
        if let Err(error) = execute_backend(&mut report) {
            eprintln!("error: {error}");
            return 1;
        }
    }
    if let Some(path) = args.receipt_out.as_deref() {
        if let Err(error) = write_receipt(path, &report) {
            eprintln!("error: {error}");
            return 1;
        }
    }
    render_embedded_lora_report(
        &report,
        LORA_TRAIN_PAYLOAD_ENV,
        LORA_TRAIN_PAYLOAD_PRETTY_ENV,
        "models/lora_train",
        args.json,
        "LoRA train",
    )
    .await
}

fn train_report(args: &ModelsLoraTrainArgs) -> Result<LoraTrainReport, String> {
    let method = normalize_lora_method(&args.method)?;
    let trainer = normalize_lora_trainer(&args.trainer)?;
    let rank = normalize_lora_rank(args.rank)?;
    let alpha = normalize_lora_alpha(args.alpha, rank)?;
    let dropout = normalize_lora_dropout(args.dropout)?;
    let requested_tool_format = normalize_plan_tool_format(&args.tool_format)?;
    let modules_to_save = normalize_modules_to_save(&args.modules_to_save)?;
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
        .or_else(|| output_dir_basename(&args.output_dir))
        .unwrap_or_else(|| "lora-adapter".to_string());
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
    )?;
    let local_runtime =
        harn_vm::llm_config::provider_config(&provider).and_then(|provider| provider.local_runtime);
    let provider_supports_lora_launch = local_runtime
        .as_ref()
        .and_then(|runtime| runtime.lora_modules_arg.as_ref())
        .is_some();
    let lora_module_value_format = lora_modules_value_format(local_runtime.as_ref());
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
    let precision = precision_contract_for_method(&method);
    let target_modules =
        target_modules_for_route(&method, &resolved.id, &resolved.family, &resolved.lineage);
    let mut warnings = Vec::new();
    let mut metadata = parse_target_metadata(&args.target_metadata)?;
    merge_serving_target_metadata(&mut metadata, &serving, &mut warnings);
    let teacher = args
        .teacher
        .as_ref()
        .map(|selector| teacher_report(selector));
    let manifest_command = post_training_manifest_command(PostTrainingManifestCommand {
        args,
        provider: &provider,
        tool_format: &decision.effective,
        adapter_name: &adapter_name,
        request_model: &request_model,
        chat_template: &chat_template,
        trainer: &trainer,
        trainer_version: args.trainer_version.as_deref(),
        method: &method,
        rank,
        alpha,
        dropout,
        metadata: &metadata,
        modules_to_save: &modules_to_save,
    });
    let eval_dataset = args.dataset.display().to_string();
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
    if let Some(correction) = &decision.correction {
        warnings.push(correction.clone());
    }
    if !args.dataset.exists() {
        warnings.push(format!(
            "dataset path does not exist: {}",
            args.dataset.display()
        ));
    }
    if args.execute && !args.dataset.exists() {
        return Err(format!(
            "--execute requires an existing trainer dataset: {}",
            args.dataset.display()
        ));
    }
    if args.execute && args.backend_argv.is_empty() {
        return Err("--execute requires backend argv after `--`".to_string());
    }
    let dataset_audit = dataset_audit_report(&args.dataset)?;
    let backend_argv_required = args.backend_argv.is_empty();
    if backend_argv_required {
        warnings.push(
            "no backend argv supplied; dry-run receipt records the named trainer contract only"
                .to_string(),
        );
    }
    let backend = BackendInvocation {
        trainer: trainer.clone(),
        argv: args.backend_argv.clone(),
        argv_required: backend_argv_required,
        cwd: args
            .backend_cwd
            .as_ref()
            .map(|path| path.display().to_string()),
        execute: args.execute,
        status: if args.execute {
            "pending".to_string()
        } else {
            "dry_run".to_string()
        },
        exit_code: None,
    };
    Ok(LoraTrainReport {
        schema_version: 1,
        producer: "harn_models_lora_train_v1".to_string(),
        ok: true,
        mode: if args.execute {
            "execute".to_string()
        } else {
            "dry_run".to_string()
        },
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
        request: TrainRequest {
            requested_tool_format,
            effective_tool_format: decision.effective.clone(),
            tool_format_correction: decision.correction,
            dataset_format: dataset_format.to_string(),
            receipt_out: args
                .receipt_out
                .as_ref()
                .map(|path| path.display().to_string()),
            execute: args.execute,
        },
        tool_calling: ToolCallingReport {
            native_tools: capabilities.native_tools,
            preferred_tool_format: capabilities.preferred_tool_format,
            text_tool_wire_format_supported: capabilities.text_tool_wire_format_supported,
            structured_output_mode: capabilities.structured_output_mode,
            recommended_endpoint: capabilities.recommended_endpoint,
        },
        contract: lora_contract_report(
            contract_id,
            &resolved.id,
            &provider,
            &decision.effective,
            dataset_format,
            Some(chat_template.clone()),
            &modules_to_save,
        ),
        training: TrainTraining {
            trainer: trainer.clone(),
            trainer_version: args.trainer_version.clone(),
            method,
            adapter_type: "peft_lora".to_string(),
            rank,
            alpha,
            dropout,
            target_modules,
            precision,
            template,
            contract: lora_training_contract(dataset_format, &decision.effective, &modules_to_save),
            trainer_contract: trainer_contract_for_dataset(
                dataset_format,
                &decision.effective,
                &trainer,
                &modules_to_save,
            ),
            max_seq_length: args.max_seq_length,
        },
        target: TrainTarget {
            adapter_name,
            request_model: request_model.clone(),
            output_dir: args.output_dir.display().to_string(),
            chat_template,
            metadata,
        },
        inputs: TrainInputs {
            dataset: path_ref(&args.dataset)?,
            corpus: args.corpus.as_deref().map(path_ref).transpose()?,
            export_manifest: args.export_manifest.as_deref().map(path_ref).transpose()?,
            teacher,
        },
        dataset_audit,
        backend,
        serving,
        promotion,
        post_training: PostTraining {
            manifest_command,
            inspect_command: vec![
                "harn".to_string(),
                "models".to_string(),
                "lora".to_string(),
                "inspect".to_string(),
                "--base".to_string(),
                args.base_model.clone(),
                "--provider".to_string(),
                provider,
                "--name".to_string(),
                request_model,
                args.output_dir.display().to_string(),
            ],
        },
        warnings,
    })
}

fn execute_backend(report: &mut LoraTrainReport) -> Result<(), String> {
    let Some(program) = report.backend.argv.first() else {
        return Err("backend argv is empty".to_string());
    };
    let mut command = Command::new(program);
    command.args(report.backend.argv.iter().skip(1));
    if let Some(cwd) = &report.backend.cwd {
        command.current_dir(cwd);
    }
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let status = command
        .status()
        .map_err(|error| format!("failed to launch backend `{program}`: {error}"))?;
    let exit_code = status.code().unwrap_or(1);
    report.backend.exit_code = Some(exit_code);
    report.backend.status = if status.success() {
        "completed".to_string()
    } else {
        "failed".to_string()
    };
    report.ok = status.success();
    Ok(())
}

fn write_receipt(path: &Path, report: &LoraTrainReport) -> Result<(), String> {
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
            .map_err(|error| format!("failed to render LoRA train receipt JSON: {error}"))?
            + "\n",
    )
    .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

struct PostTrainingManifestCommand<'a> {
    args: &'a ModelsLoraTrainArgs,
    provider: &'a str,
    tool_format: &'a str,
    adapter_name: &'a str,
    request_model: &'a str,
    chat_template: &'a str,
    trainer: &'a str,
    trainer_version: Option<&'a str>,
    method: &'a str,
    rank: u32,
    alpha: u32,
    dropout: f64,
    metadata: &'a BTreeMap<String, String>,
    modules_to_save: &'a [String],
}

fn post_training_manifest_command(ctx: PostTrainingManifestCommand<'_>) -> Vec<String> {
    let mut command = vec![
        "harn".to_string(),
        "models".to_string(),
        "lora".to_string(),
        "manifest".to_string(),
        "--base".to_string(),
        ctx.args.base_model.clone(),
        "--provider".to_string(),
        ctx.provider.to_string(),
        "--tool-format".to_string(),
        ctx.tool_format.to_string(),
        "--dataset".to_string(),
        ctx.args.dataset.display().to_string(),
        "--adapter-name".to_string(),
        ctx.adapter_name.to_string(),
        "--adapter-path".to_string(),
        ctx.args.output_dir.display().to_string(),
        "--request-model".to_string(),
        ctx.request_model.to_string(),
        "--chat-template".to_string(),
        ctx.chat_template.to_string(),
        "--trainer".to_string(),
        ctx.trainer.to_string(),
    ];
    if let Some(trainer_version) = ctx.trainer_version {
        command.extend(["--trainer-version".to_string(), trainer_version.to_string()]);
    }
    command.extend([
        "--method".to_string(),
        ctx.method.to_string(),
        "--rank".to_string(),
        ctx.rank.to_string(),
        "--alpha".to_string(),
        ctx.alpha.to_string(),
        "--dropout".to_string(),
        ctx.dropout.to_string(),
    ]);
    if let Some(corpus) = &ctx.args.corpus {
        command.extend(["--corpus".to_string(), corpus.display().to_string()]);
    }
    if let Some(export_manifest) = &ctx.args.export_manifest {
        command.extend([
            "--export-manifest".to_string(),
            export_manifest.display().to_string(),
        ]);
    }
    if let Some(teacher) = &ctx.args.teacher {
        command.extend(["--teacher".to_string(), teacher.clone()]);
    }
    for module in ctx.modules_to_save {
        command.extend(["--modules-to-save".to_string(), module.clone()]);
    }
    for (key, value) in ctx.metadata {
        command.extend(["--target-metadata".to_string(), format!("{key}={value}")]);
    }
    command
}

fn output_dir_basename(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(adapter_name_from_input)
        .filter(|name| !name.is_empty())
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

fn dataset_audit_report(path: &Path) -> Result<DatasetAudit, String> {
    if !path.exists() {
        return Ok(DatasetAudit::with_status("missing"));
    }
    if !path.is_file() {
        return Ok(DatasetAudit::with_status("not_file"));
    }
    let file = std::fs::File::open(path)
        .map_err(|error| format!("failed to open dataset {}: {error}", path.display()))?;
    let mut audit = DatasetAudit::with_status("present");
    for line in BufReader::new(file).lines() {
        let line =
            line.map_err(|error| format!("failed to read dataset {}: {error}", path.display()))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        audit.rows += 1;
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            audit.json_parse_errors += 1;
            audit.invalid_tool_block_rows += 1;
            continue;
        };
        audit_value(&mut audit, &value);
    }
    Ok(audit)
}

fn audit_value(audit: &mut DatasetAudit, value: &Value) {
    if value.get("tools").is_some() {
        audit.tool_schema_rows += 1;
    }
    if value_contains_marker(
        value,
        &["schema_repaired", "schema_repair", "repaired_schema"],
    ) {
        audit.schema_repaired_rows += 1;
    }
    if value_contains_marker(
        value,
        &["unavailable_tool", "unavailable-tool", "unknown_tool"],
    ) {
        audit.unavailable_tool_rows += 1;
    }
    let mut tool_call_count = 0_u64;
    let mut assistant_turns = 0_u64;
    let mut tool_result_turns = 0_u64;
    let mut has_native_tool_call = false;
    let mut has_parallel_tool_call = false;
    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        for message in messages {
            match message.get("role").and_then(Value::as_str) {
                Some("assistant") => {
                    assistant_turns += 1;
                    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
                        tool_call_count += tool_calls.len() as u64;
                        has_native_tool_call = true;
                        if tool_calls.len() > 1 {
                            has_parallel_tool_call = true;
                        }
                        if tool_calls.is_empty() {
                            audit.invalid_tool_block_rows += 1;
                        }
                    } else if message.get("tool_calls").is_some() {
                        audit.invalid_tool_block_rows += 1;
                    }
                }
                Some("tool") => {
                    tool_result_turns += 1;
                }
                _ => {}
            }
        }
    }
    if let Some(text) = value.get("assistant_tool_text").and_then(Value::as_str) {
        audit.assistant_tool_text_rows += 1;
        let open_count = text.matches("<tool_call").count() as u64;
        let close_count = text.matches("</tool_call>").count() as u64;
        tool_call_count += open_count;
        if open_count > 1 {
            has_parallel_tool_call = true;
        }
        if open_count != close_count {
            audit.invalid_tool_block_rows += 1;
        }
    } else if value.get("assistant_tool_text").is_some() {
        audit.invalid_tool_block_rows += 1;
    }
    if tool_call_count == 0
        && (assistant_turns > 0
            || value.get("assistant").is_some()
            || value_contains_marker(value, &["no_tool", "no-tool", "no tool"]))
    {
        audit.no_tool_rows += 1;
    }
    if has_native_tool_call {
        audit.native_tool_call_rows += 1;
    }
    if has_parallel_tool_call {
        audit.parallel_tool_call_rows += 1;
    }
    if assistant_turns > 1 || tool_result_turns > 0 {
        audit.multi_turn_rows += 1;
    }
    if tool_result_turns > 0 {
        audit.tool_result_rows += 1;
    }
}

fn value_contains_marker(value: &Value, markers: &[&str]) -> bool {
    match value {
        Value::String(text) => {
            let lowered = text.to_ascii_lowercase();
            markers.iter().any(|marker| lowered.contains(marker))
        }
        Value::Array(items) => items
            .iter()
            .any(|item| value_contains_marker(item, markers)),
        Value::Object(object) => object.iter().any(|(key, value)| {
            let lowered = key.to_ascii_lowercase();
            markers.iter().any(|marker| lowered.contains(marker))
                || value_contains_marker(value, markers)
        }),
        _ => false,
    }
}

#[derive(Debug, Serialize)]
struct LoraTrainReport {
    schema_version: u64,
    producer: String,
    ok: bool,
    mode: String,
    base: BaseModelReport,
    request: TrainRequest,
    tool_calling: ToolCallingReport,
    contract: LoraContractReport,
    training: TrainTraining,
    target: TrainTarget,
    inputs: TrainInputs,
    dataset_audit: DatasetAudit,
    backend: BackendInvocation,
    serving: ServingRecipe,
    promotion: EvaluationRecipe,
    post_training: PostTraining,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DatasetAudit {
    schema_version: u64,
    status: String,
    rows: u64,
    json_parse_errors: u64,
    invalid_tool_block_rows: u64,
    tool_schema_rows: u64,
    assistant_tool_text_rows: u64,
    native_tool_call_rows: u64,
    no_tool_rows: u64,
    unavailable_tool_rows: u64,
    schema_repaired_rows: u64,
    parallel_tool_call_rows: u64,
    multi_turn_rows: u64,
    tool_result_rows: u64,
}

impl DatasetAudit {
    fn with_status(status: &str) -> Self {
        Self {
            schema_version: 1,
            status: status.to_string(),
            rows: 0,
            json_parse_errors: 0,
            invalid_tool_block_rows: 0,
            tool_schema_rows: 0,
            assistant_tool_text_rows: 0,
            native_tool_call_rows: 0,
            no_tool_rows: 0,
            unavailable_tool_rows: 0,
            schema_repaired_rows: 0,
            parallel_tool_call_rows: 0,
            multi_turn_rows: 0,
            tool_result_rows: 0,
        }
    }
}

#[derive(Debug, Serialize)]
struct TrainRequest {
    requested_tool_format: String,
    effective_tool_format: String,
    tool_format_correction: Option<String>,
    dataset_format: String,
    receipt_out: Option<String>,
    execute: bool,
}

#[derive(Debug, Serialize)]
struct TrainTraining {
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
    trainer_contract: Vec<String>,
    max_seq_length: Option<u64>,
}

#[derive(Debug, Serialize)]
struct TrainTarget {
    adapter_name: String,
    request_model: String,
    output_dir: String,
    chat_template: String,
    metadata: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct TrainInputs {
    dataset: PathRef,
    corpus: Option<PathRef>,
    export_manifest: Option<PathRef>,
    teacher: Option<TeacherReport>,
}

#[derive(Debug, Serialize)]
struct BackendInvocation {
    trainer: String,
    argv: Vec<String>,
    argv_required: bool,
    cwd: Option<String>,
    execute: bool,
    status: String,
    exit_code: Option<i32>,
}

#[derive(Debug, Serialize)]
struct PostTraining {
    manifest_command: Vec<String>,
    inspect_command: Vec<String>,
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

    #[test]
    fn train_report_keeps_backend_launch_explicit_and_dry_run_by_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dataset = tmp.path().join("dataset.jsonl");
        std::fs::write(
            &dataset,
            "{\"assistant_tool_text\":\"<tool_call>{\\\"name\\\":\\\"edit\\\",\\\"arguments\\\":{}}</tool_call><tool_call>{\\\"name\\\":\\\"run\\\",\\\"arguments\\\":{}}</tool_call>\",\"metadata\":{\"schema_repaired\":true}}\n",
        )
        .expect("dataset");
        let args = ModelsLoraTrainArgs {
            base_model: "local-gemma4-e4b".to_string(),
            provider: Some("local-vllm".to_string()),
            tool_format: "json".to_string(),
            dataset,
            corpus: None,
            export_manifest: None,
            output_dir: tmp.path().join("adapter"),
            receipt_out: None,
            adapter_name: Some("burin-tools".to_string()),
            request_model: None,
            chat_template: None,
            trainer: "unsloth_trl_sft".to_string(),
            trainer_version: Some("unsloth-2026.7".to_string()),
            method: "qlora".to_string(),
            rank: 24,
            alpha: None,
            dropout: 0.1,
            max_seq_length: Some(8192),
            teacher: None,
            target_metadata: vec!["lane=tool-calls".to_string()],
            modules_to_save: vec!["embed_tokens".to_string()],
            execute: false,
            backend_cwd: None,
            json: true,
            backend_argv: vec![
                "uv".to_string(),
                "run".to_string(),
                "python".to_string(),
                "train.py".to_string(),
                "config.yaml".to_string(),
            ],
        };
        let report = train_report(&args).expect("report");
        assert_eq!(report.mode, "dry_run");
        assert_eq!(report.base.provider, "vllm");
        assert_eq!(report.serving.provider, "vllm");
        assert_eq!(report.training.trainer, "unsloth_sft");
        assert_eq!(
            report.training.trainer_version.as_deref(),
            Some("unsloth-2026.7")
        );
        assert_eq!(report.training.alpha, 48);
        assert_eq!(
            report.training.contract.peft_save_policy.modules_to_save,
            vec!["embed_tokens".to_string()]
        );
        assert!(
            report
                .training
                .contract
                .peft_save_policy
                .requires_weight_tying_check
        );
        assert_eq!(report.backend.status, "dry_run");
        assert!(!report.backend.execute);
        assert_eq!(report.inputs.dataset.kind, "file");
        assert_eq!(report.dataset_audit.rows, 1);
        assert_eq!(report.dataset_audit.parallel_tool_call_rows, 1);
        assert_eq!(report.dataset_audit.schema_repaired_rows, 1);
        assert_eq!(report.target.request_model, "burin-tools");
        assert!(report
            .post_training
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--provider", "vllm"]));
        assert!(report
            .post_training
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--trainer", "unsloth_sft"]));
        assert!(report
            .post_training
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--trainer-version", "unsloth-2026.7"]));
        assert!(report
            .post_training
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--modules-to-save", "embed_tokens"]));
        assert_eq!(
            report
                .target
                .metadata
                .get("serving_tool_parser_owner")
                .map(String::as_str),
            Some("harn_text_tool_parser")
        );
    }

    #[test]
    fn train_report_without_backend_argv_records_backend_requirement() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dataset = tmp.path().join("dataset.jsonl");
        std::fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
        let args = ModelsLoraTrainArgs {
            base_model: "local-gemma4-e4b".to_string(),
            provider: Some("vllm".to_string()),
            tool_format: "json".to_string(),
            dataset,
            corpus: None,
            export_manifest: None,
            output_dir: tmp.path().join("adapter"),
            receipt_out: None,
            adapter_name: None,
            request_model: None,
            chat_template: None,
            trainer: "trl_sft_trainer".to_string(),
            trainer_version: None,
            method: "qlora".to_string(),
            rank: 16,
            alpha: None,
            dropout: 0.05,
            max_seq_length: None,
            teacher: None,
            target_metadata: Vec::new(),
            modules_to_save: Vec::new(),
            execute: false,
            backend_cwd: None,
            json: true,
            backend_argv: Vec::new(),
        };
        let report = train_report(&args).expect("report");
        assert!(report.backend.argv.is_empty());
        assert!(report.backend.argv_required);
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("no backend argv supplied")));
    }
}
