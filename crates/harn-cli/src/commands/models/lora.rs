use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::cli::{ModelsLoraArgs, ModelsLoraCommand, ModelsLoraInspectArgs, ModelsLoraPlanArgs};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

const LORA_INSPECT_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_INSPECT_PAYLOAD_JSON";
const LORA_INSPECT_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_INSPECT_PAYLOAD_PRETTY";
const LORA_PLAN_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_PLAN_PAYLOAD_JSON";
const LORA_PLAN_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_PLAN_PAYLOAD_PRETTY";

/// Serialises the dispatch path so concurrent in-process callers do not race on
/// the env vars that carry the Rust-collected adapter/catalog facts.
static DISPATCH_LORA_INSPECT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn run(args: ModelsLoraArgs) {
    let exit_code = match args.command {
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
    let ok = warnings.iter().all(|warning| {
        !warning.starts_with("local adapter exists")
            && !warning.starts_with("adapter_config.json peft_type")
            && !warning.starts_with("adapter base_model_name_or_path")
    });
    let request_model = adapter.name.clone();
    let model_source = adapter
        .base_model_name_or_path
        .clone()
        .unwrap_or_else(|| resolved.id.clone());
    let lora_spec = format!("{}={}", adapter.name, adapter.input);
    let launch_provider = provider.clone();
    let max_lora_rank = adapter
        .rank
        .filter(|_| provider_supports_lora_launch && provider_supports_lora_max_rank);
    let mut harn_local_launch = vec![
        "harn".to_string(),
        "local".to_string(),
        "launch".to_string(),
        args.base_model.clone(),
        "--provider".to_string(),
        launch_provider,
        "--model-source".to_string(),
        model_source,
        "--lora-adapter".to_string(),
        lora_spec,
    ];
    if let Some(rank) = max_lora_rank {
        harn_local_launch.extend(["--max-lora-rank".to_string(), rank.to_string()]);
    }
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
        compatibility: CompatibilityReport {
            base_model_match,
            provider_supports_lora_launch,
            provider_supports_lora_max_rank,
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

fn plan_report(args: &ModelsLoraPlanArgs) -> Result<LoraPlanReport, String> {
    let method = normalize_lora_method(&args.method)?;
    let quantization = quantization_for_method(&method).to_string();
    let requested_tool_format = normalize_plan_tool_format(&args.tool_format)?;
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
    let provider_supports_lora_launch = local_runtime
        .as_ref()
        .and_then(|runtime| runtime.lora_modules_arg.as_ref())
        .is_some();
    let launch_command = if provider_supports_lora_launch {
        vec![
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
        ]
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
    let warnings = plan_warnings(
        &provider,
        &decision,
        provider_supports_lora_launch,
        capabilities.native_tools,
        &requested_tool_format,
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
            quantization,
            loss_scope: "assistant_tool_calls".to_string(),
            packing: "off_by_default_for_tool_boundaries".to_string(),
            target_modules: vec![
                "q_proj".to_string(),
                "k_proj".to_string(),
                "v_proj".to_string(),
                "o_proj".to_string(),
            ],
            notes: training_notes(&decision.effective),
        },
        data: DataRecipe {
            dataset_format: dataset_format.to_string(),
            required_columns: required_columns_for_dataset(dataset_format),
            validation: validation_steps_for_dataset(dataset_format),
        },
        evaluation: EvaluationRecipe {
            holdout_policy: "keep train/tune/holdout splits disjoint; never train on Harn eval fixtures"
                .to_string(),
            gates: vec![
                "compare base versus adapter on identical tool-call cases".to_string(),
                "track exact-call accuracy, parse failures, refusal false positives, latency, and cost"
                    .to_string(),
                "require no regression on non-tool chat smoke prompts".to_string(),
            ],
            eval_command,
        },
        launch: PlanLaunchHints {
            inspect_command,
            local_launch_command: launch_command,
            request_model,
        },
        warnings,
    })
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

fn normalize_plan_tool_format(raw: &str) -> Result<String, String> {
    let tool_format = raw.trim().to_ascii_lowercase();
    match tool_format.as_str() {
        "auto" | "native" | "text" | "json" => Ok(tool_format),
        _ => Err(format!(
            "unsupported tool format `{raw}`; expected `auto`, `native`, `text`, or `json`"
        )),
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

fn plan_warnings(
    provider: &str,
    decision: &harn_vm::llm::capabilities::ToolFormatDecision,
    provider_supports_lora_launch: bool,
    native_tools: bool,
    requested_tool_format: &str,
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
    warnings
}

#[derive(Debug, Serialize)]
struct LoraInspectReport {
    ok: bool,
    base: BaseModelReport,
    adapter: AdapterReport,
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
}

#[derive(Debug, Serialize)]
struct CompatibilityReport {
    base_model_match: BaseModelMatch,
    provider_supports_lora_launch: bool,
    provider_supports_lora_max_rank: bool,
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
    data: DataRecipe,
    evaluation: EvaluationRecipe,
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
}

#[derive(Debug, Serialize)]
struct TrainingRecipe {
    adapter_type: String,
    trainer: String,
    quantization: String,
    loss_scope: String,
    packing: String,
    target_modules: Vec<String>,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DataRecipe {
    dataset_format: String,
    required_columns: Vec<String>,
    validation: Vec<String>,
}

#[derive(Debug, Serialize)]
struct EvaluationRecipe {
    holdout_policy: String,
    gates: Vec<String>,
    eval_command: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PlanLaunchHints {
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
            json: true,
        };
        let report = inspect_report(&args).expect("report");
        assert!(!report.ok);
        assert_eq!(
            report.compatibility.base_model_match,
            BaseModelMatch::Mismatch
        );
    }
}
