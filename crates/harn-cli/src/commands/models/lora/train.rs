use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::ModelsLoraTrainArgs;

mod provenance;

use provenance::finalize_executed_trainer_provenance;

use super::{
    adapter_name_from_input, dataset_format_for_tool_format, lora_contract_id,
    lora_contract_report, lora_evaluation_recipe, lora_modules_value_format,
    lora_training_contract, merge_serving_target_metadata, normalize_lora_alpha,
    normalize_lora_dropout, normalize_lora_method, normalize_lora_rank, normalize_lora_trainer,
    normalize_modules_to_save, normalize_plan_tool_format, normalize_tool_catalog_policy,
    parse_target_metadata, precision_contract_for_method, render_embedded_report,
    resolve_lora_provider, serving_recipe, sha256_file, target_module_contract,
    target_modules_args, teacher_report, template_recipe_for_route, tool_catalog_args,
    tool_catalog_contract, trainer_contract_for_dataset, trainer_environment_check,
    trainer_identity_args, trainer_identity_check, trainer_identity_from_args, BaseModelReport,
    EvaluationRecipe, LoraContractReport, LoraContractReportInput, LoraEvaluationRecipeInput,
    LoraTrainingContract, PrecisionContract, ServingRecipe, ServingRecipeInput, TeacherReport,
    TemplateRecipe, ToolCallingReport, ToolCatalogContract, TrainerEnvironmentCheck,
    TrainerEnvironmentObservation, TrainerIdentity,
};

const LORA_TRAIN_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_TRAIN_PAYLOAD_JSON";
const LORA_TRAIN_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_TRAIN_PAYLOAD_PRETTY";
const BACKEND_RECIPE_EXPLICIT_ARGV: &str = "explicit_argv";
const BACKEND_RECIPE_HARN_LORA_SFT_V1: &str = "harn_lora_sft_v1";
const BACKEND_OUTPUT_TAIL_BYTES: usize = 120 * 1024;
const BACKEND_STREAM_READ_CHUNK_BYTES: usize = 8 * 1024;

pub(super) async fn train(args: &ModelsLoraTrainArgs) -> i32 {
    let mut report = match train_report(args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    if args.execute {
        if let Err(error) = execute_backend(&mut report, args.json) {
            eprintln!("error: {error}");
            return 1;
        }
        if let Err(error) = finalize_executed_trainer_provenance(&mut report) {
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
    render_embedded_report(
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
    let expected_trainer_identity = trainer_identity_from_args(
        args.trainer_identity.as_deref(),
        args.trainer_version.as_deref(),
    )?;
    let observed_trainer_identity = args
        .observed_trainer_identity
        .as_deref()
        .map(super::parse_trainer_identity)
        .transpose()?;
    let trainer_identity =
        trainer_identity_check(expected_trainer_identity.clone(), observed_trainer_identity);
    let trainer_environment = trainer_environment_check(expected_trainer_identity.clone(), None);
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
    let target_modules = target_module_contract(
        &args.target_modules,
        &method,
        &resolved.id,
        &resolved.family,
        &resolved.lineage,
    )?;
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
        &target_modules,
        &modules_to_save,
        &tool_catalog,
    )?;
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
    let precision = precision_contract_for_method(&method);
    let mut warnings = Vec::new();
    let mut metadata = parse_target_metadata(&args.target_metadata)?;
    merge_serving_target_metadata(&mut metadata, &serving, &mut warnings);
    let teacher = args
        .teacher
        .as_ref()
        .map(|selector| teacher_report(selector));
    let backend_plan = render_backend_plan(BackendPlanContext {
        args,
        provider: &provider,
        tool_format: &decision.effective,
        adapter_name: &adapter_name,
        request_model: &request_model,
        chat_template: &chat_template,
        trainer: &trainer,
        trainer_version: args.trainer_version.as_deref(),
        trainer_identity: expected_trainer_identity.as_ref(),
        method: &method,
        rank,
        alpha,
        dropout,
        metadata: &metadata,
        modules_to_save: &modules_to_save,
        target_modules: &target_modules,
        tool_catalog: &tool_catalog,
    })?;
    let manifest_command = post_training_manifest_command(PostTrainingManifestCommand {
        args,
        provider: &provider,
        tool_format: &decision.effective,
        adapter_name: &adapter_name,
        request_model: &request_model,
        chat_template: &chat_template,
        trainer: &trainer,
        trainer_version: args.trainer_version.as_deref(),
        trainer_identity: expected_trainer_identity.as_ref(),
        method: &method,
        rank,
        alpha,
        dropout,
        metadata: &metadata,
        modules_to_save: &modules_to_save,
        target_modules: &target_modules,
        tool_catalog: &tool_catalog,
    });
    let eval_dataset = args.dataset.display().to_string();
    let promotion = lora_evaluation_recipe(LoraEvaluationRecipeInput {
        contract_id: &contract_id,
        base_model: &resolved.id,
        provider: &provider,
        request_model: &request_model,
        tool_format: &decision.effective,
        eval_dataset: &eval_dataset,
        trainer_identity: Some(&trainer_identity),
        trainer_environment: Some(&trainer_environment),
        eval_command: vec![
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
    });
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
    if args.execute && backend_plan.argv.is_empty() {
        return Err(
            "--execute requires backend argv after `--` or a backend recipe that renders argv"
                .to_string(),
        );
    }
    if args.execute && expected_trainer_identity.is_none() {
        return Err("--execute requires --trainer-identity or --trainer-version so the trainer stack is reproducible".to_string());
    }
    let dataset_audit = dataset_audit_report(&args.dataset)?;
    let backend_argv_required = backend_plan.argv.is_empty();
    if backend_argv_required {
        warnings.push(
            "no backend argv supplied; dry-run receipt records the named trainer contract only"
                .to_string(),
        );
    }
    if !trainer_identity.promotable {
        warnings.push(
            "trainer identity is not promotable until expected and observed identities match"
                .to_string(),
        );
    }
    let backend = BackendInvocation {
        trainer: trainer.clone(),
        recipe: backend_plan.recipe,
        argv_source: backend_plan.argv_source,
        argv: backend_plan.argv,
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
        trainer_identity_path: default_trainer_identity_path(&args.output_dir)
            .display()
            .to_string(),
        result_path: backend_plan
            .result_path
            .as_ref()
            .map(|path| path.display().to_string()),
        result: None,
        exit_code: None,
        output_tail_bytes: BACKEND_OUTPUT_TAIL_BYTES,
        stdout_tail: None,
        stderr_tail: None,
        stdout_tail_truncated: false,
        stderr_tail_truncated: false,
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
            tool_catalog_policy: tool_catalog.policy.clone(),
            tool_catalog_id: tool_catalog.catalog_id.clone(),
            tool_catalog_hash: tool_catalog.catalog_hash.clone(),
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
        contract: lora_contract_report(LoraContractReportInput {
            contract_id,
            base_model: &resolved.id,
            provider: &provider,
            harn_tool_format: &decision.effective,
            dataset_format,
            chat_template: Some(chat_template.clone()),
            target_modules: &target_modules,
            modules_to_save: &modules_to_save,
            tool_catalog: &tool_catalog,
        }),
        training: TrainTraining {
            trainer: trainer.clone(),
            trainer_version: args.trainer_version.clone(),
            trainer_identity,
            trainer_environment,
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
            trainer_contract: trainer_contract_for_dataset(
                dataset_format,
                &decision.effective,
                &trainer,
                &modules_to_save,
                &tool_catalog,
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

struct BackendPlanContext<'a> {
    args: &'a ModelsLoraTrainArgs,
    provider: &'a str,
    tool_format: &'a str,
    adapter_name: &'a str,
    request_model: &'a str,
    chat_template: &'a str,
    trainer: &'a str,
    trainer_version: Option<&'a str>,
    trainer_identity: Option<&'a TrainerIdentity>,
    method: &'a str,
    rank: u32,
    alpha: u32,
    dropout: f64,
    metadata: &'a BTreeMap<String, String>,
    modules_to_save: &'a [String],
    target_modules: &'a super::TargetModuleContract,
    tool_catalog: &'a ToolCatalogContract,
}

struct RenderedBackendPlan {
    recipe: String,
    argv_source: String,
    argv: Vec<String>,
    result_path: Option<std::path::PathBuf>,
}

fn render_backend_plan(ctx: BackendPlanContext<'_>) -> Result<RenderedBackendPlan, String> {
    let recipe = normalize_backend_recipe(&ctx.args.backend_recipe)?;
    match recipe.as_str() {
        BACKEND_RECIPE_EXPLICIT_ARGV => {
            if !ctx.args.backend_runner.is_empty()
                || ctx.args.backend_script.is_some()
                || ctx.args.backend_config.is_some()
            {
                return Err(format!(
                    "--backend-runner, --backend-script, and --backend-config require --backend-recipe {BACKEND_RECIPE_HARN_LORA_SFT_V1}"
                ));
            }
            Ok(RenderedBackendPlan {
                recipe,
                argv_source: "explicit".to_string(),
                argv: ctx.args.backend_argv.clone(),
                result_path: ctx.args.backend_result_out.clone(),
            })
        }
        BACKEND_RECIPE_HARN_LORA_SFT_V1 => {
            if !ctx.args.backend_argv.is_empty() {
                return Err(format!(
                    "backend argv after `--` cannot be combined with --backend-recipe {BACKEND_RECIPE_HARN_LORA_SFT_V1}; use --backend-runner, --backend-script, and --backend-config"
                ));
            }
            let script = ctx.args.backend_script.as_deref().ok_or_else(|| {
                format!(
                    "--backend-recipe {BACKEND_RECIPE_HARN_LORA_SFT_V1} requires --backend-script"
                )
            })?;
            let mut argv = if ctx.args.backend_runner.is_empty() {
                vec!["python".to_string()]
            } else {
                ctx.args.backend_runner.clone()
            };
            argv.push(script.display().to_string());
            argv.extend([
                "--dataset".to_string(),
                ctx.args.dataset.display().to_string(),
                "--output-dir".to_string(),
                ctx.args.output_dir.display().to_string(),
                "--base".to_string(),
                ctx.args.base_model.clone(),
                "--provider".to_string(),
                ctx.provider.to_string(),
                "--tool-format".to_string(),
                ctx.tool_format.to_string(),
                "--adapter-name".to_string(),
                ctx.adapter_name.to_string(),
                "--request-model".to_string(),
                ctx.request_model.to_string(),
                "--chat-template".to_string(),
                ctx.chat_template.to_string(),
                "--trainer".to_string(),
                ctx.trainer.to_string(),
                "--method".to_string(),
                ctx.method.to_string(),
                "--rank".to_string(),
                ctx.rank.to_string(),
                "--alpha".to_string(),
                ctx.alpha.to_string(),
                "--dropout".to_string(),
                ctx.dropout.to_string(),
            ]);
            if let Some(trainer_version) = ctx.trainer_version {
                argv.extend(["--trainer-version".to_string(), trainer_version.to_string()]);
            }
            argv.extend(trainer_identity_args(ctx.trainer_identity));
            argv.extend([
                "--trainer-identity-out".to_string(),
                default_trainer_identity_path(&ctx.args.output_dir)
                    .display()
                    .to_string(),
            ]);
            let result_path = ctx
                .args
                .backend_result_out
                .clone()
                .unwrap_or_else(|| default_backend_result_path(&ctx.args.output_dir));
            argv.extend([
                "--backend-result-out".to_string(),
                result_path.display().to_string(),
            ]);
            if let Some(max_seq_length) = ctx.args.max_seq_length {
                argv.extend(["--max-seq-length".to_string(), max_seq_length.to_string()]);
            }
            if let Some(corpus) = &ctx.args.corpus {
                argv.extend(["--corpus".to_string(), corpus.display().to_string()]);
            }
            if let Some(export_manifest) = &ctx.args.export_manifest {
                argv.extend([
                    "--export-manifest".to_string(),
                    export_manifest.display().to_string(),
                ]);
            }
            if let Some(teacher) = &ctx.args.teacher {
                argv.extend(["--teacher".to_string(), teacher.clone()]);
            }
            for module in ctx.modules_to_save {
                argv.extend(["--modules-to-save".to_string(), module.clone()]);
            }
            argv.extend(target_modules_args(ctx.target_modules));
            argv.extend(tool_catalog_args(ctx.tool_catalog));
            for (key, value) in ctx.metadata {
                argv.extend(["--target-metadata".to_string(), format!("{key}={value}")]);
            }
            if let Some(config) = &ctx.args.backend_config {
                argv.extend(["--config".to_string(), config.display().to_string()]);
            }
            Ok(RenderedBackendPlan {
                recipe,
                argv_source: "recipe".to_string(),
                argv,
                result_path: Some(result_path),
            })
        }
        _ => unreachable!("normalize_backend_recipe returned an unsupported recipe"),
    }
}

fn normalize_backend_recipe(input: &str) -> Result<String, String> {
    match input.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "" | "explicit" | "explicit_argv" | "external_argv" | "argv" => {
            Ok(BACKEND_RECIPE_EXPLICIT_ARGV.to_string())
        }
        "harn_lora_sft_v1" | "harn_sft_v1" | "canonical_sft_v1" => {
            Ok(BACKEND_RECIPE_HARN_LORA_SFT_V1.to_string())
        }
        other => Err(format!(
            "unsupported LoRA backend recipe `{other}`; expected {BACKEND_RECIPE_EXPLICIT_ARGV} or {BACKEND_RECIPE_HARN_LORA_SFT_V1}"
        )),
    }
}

fn execute_backend(report: &mut LoraTrainReport, json_output: bool) -> Result<(), String> {
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to launch backend `{program}`: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("failed to capture backend `{program}` stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("failed to capture backend `{program}` stderr"))?;
    let stdout_tail = Arc::new(Mutex::new(OutputTail::new(BACKEND_OUTPUT_TAIL_BYTES)));
    let stderr_tail = Arc::new(Mutex::new(OutputTail::new(BACKEND_OUTPUT_TAIL_BYTES)));
    // A structured CLI response owns stdout; backend diagnostics stay in the receipt.
    let stdout_handle = tee_backend_stream(stdout, false, !json_output, Arc::clone(&stdout_tail));
    let stderr_handle = tee_backend_stream(stderr, true, true, Arc::clone(&stderr_tail));
    let status = child
        .wait()
        .map_err(|error| format!("failed to wait for backend `{program}`: {error}"))?;
    let exit_code = status.code().unwrap_or(1);
    report.backend.exit_code = Some(exit_code);
    report.backend.status = if status.success() {
        "completed".to_string()
    } else {
        "failed".to_string()
    };
    report.ok = status.success();
    if let Some(warning) = join_stream_thread(stdout_handle, "stdout") {
        report.warnings.push(warning);
    }
    if let Some(warning) = join_stream_thread(stderr_handle, "stderr") {
        report.warnings.push(warning);
    }
    let stdout = output_tail(stdout_tail, "stdout");
    report.backend.stdout_tail = stdout.text;
    report.backend.stdout_tail_truncated = stdout.truncated;
    let stderr = output_tail(stderr_tail, "stderr");
    report.backend.stderr_tail = stderr.text;
    report.backend.stderr_tail_truncated = stderr.truncated;
    finalize_backend_result(report, status.success())?;
    Ok(())
}

fn finalize_backend_result(
    report: &mut LoraTrainReport,
    backend_succeeded: bool,
) -> Result<(), String> {
    let Some(result_path) = report.backend.result_path.clone() else {
        return Ok(());
    };
    let path = Path::new(&result_path);
    if !path.exists() {
        if backend_succeeded {
            report.ok = false;
            report.backend.status = "completed_missing_backend_result".to_string();
            report.warnings.push(format!(
                "backend completed but did not write typed result: {}",
                path.display()
            ));
        }
        return Ok(());
    }
    let result = read_backend_result(path)?;
    match apply_backend_result(report, result) {
        Ok(()) => Ok(()),
        Err(error) if report.backend.status == "completed_metadata_conflict" => {
            report.warnings.push(error);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn read_backend_result(path: &Path) -> Result<BackendResult, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read backend result {}: {error}", path.display()))?;
    let result: BackendResult = serde_json::from_str(&text)
        .map_err(|error| format!("failed to parse backend result {}: {error}", path.display()))?;
    if result.schema_version != 1 {
        return Err(format!(
            "unsupported backend result schema_version {} in {}; expected 1",
            result.schema_version,
            path.display()
        ));
    }
    Ok(result)
}

fn apply_backend_result(report: &mut LoraTrainReport, result: BackendResult) -> Result<(), String> {
    for (key, value) in &result.target_metadata {
        if let Some(existing) = report.target.metadata.get(key) {
            if existing != value {
                report.ok = false;
                report.backend.status = "completed_metadata_conflict".to_string();
                return Err(format!(
                    "backend result target metadata conflict for `{key}`: Harn planned `{existing}` but backend reported `{value}`"
                ));
            }
        }
    }
    for (key, value) in &result.target_metadata {
        if report
            .target
            .metadata
            .insert(key.clone(), value.clone())
            .is_none()
        {
            report
                .post_training
                .manifest_command
                .extend(["--target-metadata".to_string(), format!("{key}={value}")]);
        }
    }
    if let Some(observed) = result.trainer_identity.clone() {
        report.training.trainer_identity = trainer_identity_check(
            report.training.trainer_identity.expected.clone(),
            Some(observed),
        );
    }
    report.warnings.extend(
        result
            .warnings
            .iter()
            .map(|warning| format!("backend result: {warning}")),
    );
    report.backend.result = Some(result);
    Ok(())
}

fn default_trainer_identity_path(output_dir: &Path) -> std::path::PathBuf {
    output_dir.join("trainer.identity.json")
}

fn default_backend_result_path(output_dir: &Path) -> std::path::PathBuf {
    output_dir.join("backend.result.json")
}

fn tee_backend_stream<R: Read + Send + 'static>(
    stream: R,
    is_stderr: bool,
    mirror: bool,
    tail: Arc<Mutex<OutputTail>>,
) -> thread::JoinHandle<Result<(), String>> {
    thread::spawn(move || {
        if !mirror {
            return capture_backend_stream(stream, &mut std::io::sink(), tail, "stdout");
        }
        if is_stderr {
            let mut out = std::io::stderr().lock();
            capture_backend_stream(stream, &mut out, tail, "stderr")
        } else {
            let mut out = std::io::stdout().lock();
            capture_backend_stream(stream, &mut out, tail, "stdout")
        }
    })
}

fn capture_backend_stream<R: Read, W: Write>(
    mut stream: R,
    mirror: &mut W,
    tail: Arc<Mutex<OutputTail>>,
    stream_name: &str,
) -> Result<(), String> {
    let mut chunk = [0_u8; BACKEND_STREAM_READ_CHUNK_BYTES];
    let mut mirror_error = None;
    loop {
        let read = stream
            .read(&mut chunk)
            .map_err(|error| format!("failed to read backend stream: {error}"))?;
        if read == 0 {
            break;
        }
        let bytes = &chunk[..read];
        tail.lock()
            .map_err(|_| "backend output tail lock was poisoned".to_string())?
            .push(bytes);
        if mirror_error.is_none() {
            if let Err(error) = mirror.write_all(bytes).and_then(|_| mirror.flush()) {
                mirror_error = Some(format!("failed to mirror backend {stream_name}: {error}"));
            }
        }
    }
    mirror_error.map_or(Ok(()), Err)
}

fn join_stream_thread(
    handle: thread::JoinHandle<Result<(), String>>,
    stream_name: &str,
) -> Option<String> {
    match handle.join() {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(format!("backend {stream_name} capture warning: {error}")),
        Err(_) => Some(format!("backend {stream_name} capture thread panicked")),
    }
}

struct CapturedTail {
    text: Option<String>,
    truncated: bool,
}

fn output_tail(tail: Arc<Mutex<OutputTail>>, stream_name: &str) -> CapturedTail {
    match tail.lock() {
        Ok(tail) => CapturedTail {
            text: tail.text(),
            truncated: tail.truncated,
        },
        Err(_) => CapturedTail {
            text: Some(format!("<failed to read {stream_name} output tail>")),
            truncated: false,
        },
    }
}

#[derive(Debug)]
struct OutputTail {
    bytes: Vec<u8>,
    limit: usize,
    truncated: bool,
}

impl OutputTail {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            truncated: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if chunk.is_empty() || self.limit == 0 {
            return;
        }
        if chunk.len() >= self.limit {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&chunk[chunk.len() - self.limit..]);
            self.truncated = true;
            return;
        }
        let overflow = self.bytes.len() + chunk.len();
        if overflow > self.limit {
            self.bytes.drain(0..(overflow - self.limit));
            self.truncated = true;
        }
        self.bytes.extend_from_slice(chunk);
    }

    fn text(&self) -> Option<String> {
        if self.bytes.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&self.bytes).into_owned())
    }
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
    trainer_identity: Option<&'a TrainerIdentity>,
    method: &'a str,
    rank: u32,
    alpha: u32,
    dropout: f64,
    metadata: &'a BTreeMap<String, String>,
    modules_to_save: &'a [String],
    target_modules: &'a super::TargetModuleContract,
    tool_catalog: &'a ToolCatalogContract,
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
    command.extend(trainer_identity_args(ctx.trainer_identity));
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
    command.extend(target_modules_args(ctx.target_modules));
    command.extend(tool_catalog_args(ctx.tool_catalog));
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
    tool_catalog_policy: String,
    tool_catalog_id: Option<String>,
    tool_catalog_hash: Option<String>,
    receipt_out: Option<String>,
    execute: bool,
}

#[derive(Debug, Serialize)]
struct TrainTraining {
    trainer: String,
    trainer_version: Option<String>,
    trainer_identity: super::TrainerIdentityCheck,
    trainer_environment: TrainerEnvironmentCheck,
    method: String,
    adapter_type: String,
    rank: u32,
    alpha: u32,
    dropout: f64,
    target_modules: super::TargetModuleContract,
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
    recipe: String,
    argv_source: String,
    argv: Vec<String>,
    argv_required: bool,
    cwd: Option<String>,
    execute: bool,
    status: String,
    trainer_identity_path: String,
    result_path: Option<String>,
    result: Option<BackendResult>,
    exit_code: Option<i32>,
    output_tail_bytes: usize,
    stdout_tail: Option<String>,
    stderr_tail: Option<String>,
    stdout_tail_truncated: bool,
    stderr_tail_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BackendResult {
    schema_version: u64,
    rendered_records: Option<u64>,
    trainable_records: Option<u64>,
    retention_ratio: Option<f64>,
    trainer_identity: Option<TrainerIdentity>,
    #[serde(default)]
    trainer_environment_observation: Option<TrainerEnvironmentObservation>,
    #[serde(default)]
    runtime: BTreeMap<String, Value>,
    #[serde(default)]
    tokenizer: BTreeMap<String, Value>,
    #[serde(default)]
    artifacts: BTreeMap<String, String>,
    #[serde(default)]
    target_metadata: BTreeMap<String, String>,
    #[serde(default)]
    warnings: Vec<String>,
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
    use std::io::Cursor;

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
            trainer_identity: None,
            observed_trainer_identity: Some("version=unsloth-2026.7".to_string()),
            method: "qlora".to_string(),
            rank: 24,
            alpha: None,
            dropout: 0.1,
            max_seq_length: Some(8192),
            teacher: None,
            target_metadata: vec!["lane=tool-calls".to_string()],
            tool_catalog_policy: "full_schema".to_string(),
            tool_catalog_id: None,
            tool_catalog_hash: None,
            modules_to_save: vec!["embed_tokens".to_string()],
            target_modules: Vec::new(),
            backend_recipe: "explicit_argv".to_string(),
            backend_runner: Vec::new(),
            backend_script: None,
            backend_config: None,
            backend_result_out: None,
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
        assert_eq!(report.backend.recipe, "explicit_argv");
        assert_eq!(report.backend.argv_source, "explicit");
        assert_eq!(report.backend.status, "dry_run");
        assert!(!report.backend.execute);
        assert_eq!(report.backend.output_tail_bytes, BACKEND_OUTPUT_TAIL_BYTES);
        assert!(report.backend.stdout_tail.is_none());
        assert!(report.backend.stderr_tail.is_none());
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
            trainer_identity: None,
            observed_trainer_identity: None,
            method: "qlora".to_string(),
            rank: 16,
            alpha: None,
            dropout: 0.05,
            max_seq_length: None,
            teacher: None,
            target_metadata: Vec::new(),
            tool_catalog_policy: "full_schema".to_string(),
            tool_catalog_id: None,
            tool_catalog_hash: None,
            modules_to_save: Vec::new(),
            target_modules: Vec::new(),
            backend_recipe: "explicit_argv".to_string(),
            backend_runner: Vec::new(),
            backend_script: None,
            backend_config: None,
            backend_result_out: None,
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

    #[test]
    fn train_report_renders_harn_lora_sft_recipe_backend_argv() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dataset = tmp.path().join("dataset.jsonl");
        let export_manifest = tmp.path().join("export.manifest.json");
        std::fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
        std::fs::write(&export_manifest, "{}\n").expect("manifest");
        let args = ModelsLoraTrainArgs {
            base_model: "local-gemma4-e4b".to_string(),
            provider: Some("vllm".to_string()),
            tool_format: "json".to_string(),
            dataset: dataset.clone(),
            corpus: Some(tmp.path().join("corpus")),
            export_manifest: Some(export_manifest.clone()),
            output_dir: tmp.path().join("adapter"),
            receipt_out: None,
            adapter_name: Some("burin-tools".to_string()),
            request_model: Some("burin-tools".to_string()),
            chat_template: Some("harn_text_tool_calls_json_fences".to_string()),
            trainer: "unsloth_sft".to_string(),
            trainer_version: Some("unsloth-2026.7".to_string()),
            trainer_identity: None,
            observed_trainer_identity: Some("version=unsloth-2026.7".to_string()),
            method: "lora".to_string(),
            rank: 32,
            alpha: Some(64),
            dropout: 0.1,
            max_seq_length: Some(8192),
            teacher: Some("dashscope/qwen3-coder-next".to_string()),
            target_metadata: vec!["lane=structured".to_string()],
            tool_catalog_policy: "fixed_catalog_internalized".to_string(),
            tool_catalog_id: Some("burin-tools-v1".to_string()),
            tool_catalog_hash: Some("sha256:burin-tool-catalog".to_string()),
            modules_to_save: vec!["embed_tokens".to_string(), "lm_head".to_string()],
            target_modules: vec!["q_proj".to_string(), "v_proj".to_string()],
            backend_recipe: "harn_lora_sft_v1".to_string(),
            backend_runner: vec!["uv".to_string(), "run".to_string(), "python".to_string()],
            backend_script: Some("train.py".into()),
            backend_config: Some("config/e4b.yaml".into()),
            backend_result_out: Some(tmp.path().join("backend-result.json")),
            execute: false,
            backend_cwd: Some(tmp.path().join("trainer")),
            json: true,
            backend_argv: Vec::new(),
        };
        let report = train_report(&args).expect("report");

        assert_eq!(report.backend.recipe, "harn_lora_sft_v1");
        assert_eq!(report.backend.argv_source, "recipe");
        let expected_cwd = tmp.path().join("trainer").display().to_string();
        assert_eq!(report.backend.cwd.as_deref(), Some(expected_cwd.as_str()));
        let first_four: Vec<&str> = report
            .backend
            .argv
            .iter()
            .take(4)
            .map(String::as_str)
            .collect();
        assert_eq!(first_four, vec!["uv", "run", "python", "train.py"]);
        let expected_pairs = vec![
            ("--dataset".to_string(), dataset.display().to_string()),
            (
                "--output-dir".to_string(),
                tmp.path().join("adapter").display().to_string(),
            ),
            ("--base".to_string(), "local-gemma4-e4b".to_string()),
            ("--provider".to_string(), "vllm".to_string()),
            ("--tool-format".to_string(), "json".to_string()),
            ("--adapter-name".to_string(), "burin-tools".to_string()),
            ("--request-model".to_string(), "burin-tools".to_string()),
            (
                "--chat-template".to_string(),
                "harn_text_tool_calls_json_fences".to_string(),
            ),
            ("--trainer".to_string(), "unsloth_sft".to_string()),
            (
                "--trainer-version".to_string(),
                "unsloth-2026.7".to_string(),
            ),
            ("--method".to_string(), "lora".to_string()),
            ("--rank".to_string(), "32".to_string()),
            ("--alpha".to_string(), "64".to_string()),
            ("--dropout".to_string(), "0.1".to_string()),
            ("--max-seq-length".to_string(), "8192".to_string()),
            (
                "--corpus".to_string(),
                tmp.path().join("corpus").display().to_string(),
            ),
            (
                "--export-manifest".to_string(),
                export_manifest.display().to_string(),
            ),
            (
                "--teacher".to_string(),
                "dashscope/qwen3-coder-next".to_string(),
            ),
            (
                "--tool-catalog-policy".to_string(),
                "fixed_catalog_internalized".to_string(),
            ),
            (
                "--tool-catalog-id".to_string(),
                "burin-tools-v1".to_string(),
            ),
            (
                "--tool-catalog-hash".to_string(),
                "sha256:burin-tool-catalog".to_string(),
            ),
            (
                "--target-metadata".to_string(),
                "lane=structured".to_string(),
            ),
            (
                "--backend-result-out".to_string(),
                tmp.path().join("backend-result.json").display().to_string(),
            ),
            ("--config".to_string(), "config/e4b.yaml".to_string()),
        ];
        for (flag, value) in expected_pairs {
            assert!(
                report
                    .backend
                    .argv
                    .windows(2)
                    .any(|pair| pair[0] == flag && pair[1] == value),
                "missing backend argv pair: {flag} {value}"
            );
        }
        assert_eq!(
            report
                .backend
                .argv
                .windows(2)
                .filter(|pair| *pair == ["--modules-to-save", "embed_tokens"])
                .count(),
            1
        );
        assert_eq!(
            report
                .backend
                .argv
                .windows(2)
                .filter(|pair| *pair == ["--modules-to-save", "lm_head"])
                .count(),
            1
        );
        assert_eq!(report.training.target_modules.policy, "explicit");
        let expected_result_path = tmp.path().join("backend-result.json").display().to_string();
        assert_eq!(
            report.backend.result_path.as_deref(),
            Some(expected_result_path.as_str())
        );
        assert_eq!(
            report.training.target_modules.modules,
            vec!["q_proj".to_string(), "v_proj".to_string()]
        );
        for module in ["q_proj", "v_proj"] {
            assert_eq!(
                report
                    .backend
                    .argv
                    .windows(2)
                    .filter(|pair| *pair == ["--target-modules", module])
                    .count(),
                1
            );
            assert_eq!(
                report
                    .post_training
                    .manifest_command
                    .windows(2)
                    .filter(|pair| *pair == ["--target-modules", module])
                    .count(),
                1
            );
        }
    }

    #[test]
    fn backend_result_merges_runtime_metadata_into_harn_manifest_receipt() {
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
            adapter_name: Some("burin-tools".to_string()),
            request_model: None,
            chat_template: None,
            trainer: "unsloth_sft".to_string(),
            trainer_version: Some("unsloth-2026.7".to_string()),
            trainer_identity: None,
            observed_trainer_identity: None,
            method: "qlora".to_string(),
            rank: 16,
            alpha: None,
            dropout: 0.05,
            max_seq_length: None,
            teacher: None,
            target_metadata: vec!["lane=structured".to_string()],
            tool_catalog_policy: "full_schema".to_string(),
            tool_catalog_id: None,
            tool_catalog_hash: None,
            modules_to_save: Vec::new(),
            target_modules: Vec::new(),
            backend_recipe: "explicit_argv".to_string(),
            backend_runner: Vec::new(),
            backend_script: None,
            backend_config: None,
            backend_result_out: Some(tmp.path().join("backend.result.json")),
            execute: false,
            backend_cwd: None,
            json: true,
            backend_argv: Vec::new(),
        };
        let mut report = train_report(&args).expect("report");
        let result = BackendResult {
            schema_version: 1,
            rendered_records: Some(197),
            trainable_records: Some(190),
            retention_ratio: Some(0.964),
            trainer_identity: Some(TrainerIdentity {
                schema_version: 1,
                kind: "version".to_string(),
                value: "unsloth-2026.7".to_string(),
            }),
            trainer_environment_observation: None,
            runtime: std::collections::BTreeMap::from([(
                "torch_version".to_string(),
                serde_json::Value::String("2.9.0".to_string()),
            )]),
            tokenizer: std::collections::BTreeMap::from([(
                "class".to_string(),
                serde_json::Value::String("GemmaTokenizerFast".to_string()),
            )]),
            artifacts: std::collections::BTreeMap::from([(
                "adapter_dir".to_string(),
                tmp.path().join("adapter").display().to_string(),
            )]),
            target_metadata: std::collections::BTreeMap::from([
                ("lane".to_string(), "structured".to_string()),
                ("rendered_records".to_string(), "197".to_string()),
                ("trainable_records".to_string(), "190".to_string()),
                ("retention_ratio".to_string(), "0.964".to_string()),
            ]),
            warnings: vec!["tokenizer added a pad token".to_string()],
        };

        apply_backend_result(&mut report, result).expect("backend result");

        assert_eq!(
            report
                .target
                .metadata
                .get("trainable_records")
                .map(String::as_str),
            Some("190")
        );
        assert!(report
            .post_training
            .manifest_command
            .windows(2)
            .any(|pair| pair == ["--target-metadata", "retention_ratio=0.964"]));
        assert_eq!(
            report
                .backend
                .result
                .as_ref()
                .and_then(|result| result.trainable_records),
            Some(190)
        );
        assert_eq!(report.training.trainer_identity.status, "matched");
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning == "backend result: tokenizer added a pad token"));
    }

    #[test]
    fn backend_result_rejects_conflicting_harn_planned_metadata() {
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
            trainer: "unsloth_sft".to_string(),
            trainer_version: Some("unsloth-2026.7".to_string()),
            trainer_identity: None,
            observed_trainer_identity: None,
            method: "qlora".to_string(),
            rank: 16,
            alpha: None,
            dropout: 0.05,
            max_seq_length: None,
            teacher: None,
            target_metadata: vec!["lane=structured".to_string()],
            tool_catalog_policy: "full_schema".to_string(),
            tool_catalog_id: None,
            tool_catalog_hash: None,
            modules_to_save: Vec::new(),
            target_modules: Vec::new(),
            backend_recipe: "explicit_argv".to_string(),
            backend_runner: Vec::new(),
            backend_script: None,
            backend_config: None,
            backend_result_out: None,
            execute: false,
            backend_cwd: None,
            json: true,
            backend_argv: Vec::new(),
        };
        let mut report = train_report(&args).expect("report");
        let result = BackendResult {
            schema_version: 1,
            rendered_records: None,
            trainable_records: None,
            retention_ratio: None,
            trainer_identity: None,
            trainer_environment_observation: None,
            runtime: std::collections::BTreeMap::new(),
            tokenizer: std::collections::BTreeMap::new(),
            artifacts: std::collections::BTreeMap::new(),
            target_metadata: std::collections::BTreeMap::from([(
                "lane".to_string(),
                "backend-overrode-harn".to_string(),
            )]),
            warnings: Vec::new(),
        };

        let error = apply_backend_result(&mut report, result).expect_err("metadata conflict");

        assert!(error.contains("target metadata conflict"));
        assert!(!report.ok);
        assert_eq!(report.backend.status, "completed_metadata_conflict");
    }

    #[test]
    fn train_report_rejects_recipe_options_in_explicit_mode() {
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
            trainer: "external_sft_trainer".to_string(),
            trainer_version: None,
            trainer_identity: None,
            observed_trainer_identity: None,
            method: "qlora".to_string(),
            rank: 16,
            alpha: None,
            dropout: 0.05,
            max_seq_length: None,
            teacher: None,
            target_metadata: Vec::new(),
            tool_catalog_policy: "full_schema".to_string(),
            tool_catalog_id: None,
            tool_catalog_hash: None,
            modules_to_save: Vec::new(),
            target_modules: Vec::new(),
            backend_recipe: "explicit_argv".to_string(),
            backend_runner: vec!["uv".to_string()],
            backend_script: None,
            backend_config: None,
            backend_result_out: None,
            execute: false,
            backend_cwd: None,
            json: true,
            backend_argv: Vec::new(),
        };

        let error = train_report(&args).expect_err("recipe option should fail explicit mode");
        assert!(error.contains("require --backend-recipe harn_lora_sft_v1"));
    }

    #[test]
    fn train_report_rejects_recipe_without_script() {
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
            trainer: "external_sft_trainer".to_string(),
            trainer_version: None,
            trainer_identity: None,
            observed_trainer_identity: None,
            method: "qlora".to_string(),
            rank: 16,
            alpha: None,
            dropout: 0.05,
            max_seq_length: None,
            teacher: None,
            target_metadata: Vec::new(),
            tool_catalog_policy: "full_schema".to_string(),
            tool_catalog_id: None,
            tool_catalog_hash: None,
            modules_to_save: Vec::new(),
            target_modules: Vec::new(),
            backend_recipe: "harn-lora-sft-v1".to_string(),
            backend_runner: Vec::new(),
            backend_script: None,
            backend_config: None,
            backend_result_out: None,
            execute: false,
            backend_cwd: None,
            json: true,
            backend_argv: Vec::new(),
        };

        let error = train_report(&args).expect_err("recipe without script should fail");
        assert!(error.contains("requires --backend-script"));
    }

    #[test]
    fn train_report_rejects_raw_backend_argv_in_recipe_mode() {
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
            trainer: "external_sft_trainer".to_string(),
            trainer_version: None,
            trainer_identity: None,
            observed_trainer_identity: None,
            method: "qlora".to_string(),
            rank: 16,
            alpha: None,
            dropout: 0.05,
            max_seq_length: None,
            teacher: None,
            target_metadata: Vec::new(),
            tool_catalog_policy: "full_schema".to_string(),
            tool_catalog_id: None,
            tool_catalog_hash: None,
            modules_to_save: Vec::new(),
            target_modules: Vec::new(),
            backend_recipe: "harn_lora_sft_v1".to_string(),
            backend_runner: Vec::new(),
            backend_script: Some("train.py".into()),
            backend_config: None,
            backend_result_out: None,
            execute: false,
            backend_cwd: None,
            json: true,
            backend_argv: vec!["python".to_string(), "legacy_train.py".to_string()],
        };

        let error = train_report(&args).expect_err("recipe with raw argv should fail");
        assert!(error.contains("cannot be combined"));
    }

    #[test]
    fn output_tail_keeps_bounded_suffix() {
        let mut tail = OutputTail::new(5);
        tail.push(b"abc");
        assert_eq!(tail.text().as_deref(), Some("abc"));
        assert!(!tail.truncated);
        tail.push(b"def");
        assert_eq!(tail.text().as_deref(), Some("bcdef"));
        assert!(tail.truncated);
        tail.push(b"ghijkl");
        assert_eq!(tail.text().as_deref(), Some("hijkl"));
        assert!(tail.truncated);
    }

    #[test]
    fn backend_stream_capture_keeps_bounded_suffix_for_unbroken_output() {
        let tail = Arc::new(Mutex::new(OutputTail::new(5)));
        let input = Cursor::new(b"abcdefghijkl".to_vec());
        let mut mirrored = Vec::new();

        capture_backend_stream(input, &mut mirrored, Arc::clone(&tail), "stdout").unwrap();

        assert_eq!(mirrored, b"abcdefghijkl");
        let captured = output_tail(tail, "stdout");
        assert_eq!(captured.text.as_deref(), Some("hijkl"));
        assert!(captured.truncated);
    }
}
