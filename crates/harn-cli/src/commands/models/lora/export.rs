use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead as _, BufReader, BufWriter, Write as _};
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest as _, Sha256};

use crate::cli::ModelsLoraExportArgs;

use super::{
    dataset_format_for_tool_format, expand_home, lora_adapter_binding, lora_contract_id,
    lora_contract_report, lora_evaluation_recipe, lora_modules_value_format,
    normalize_modules_to_save, normalize_plan_tool_format, parse_target_metadata,
    render_embedded_lora_report, resolve_lora_provider, sha256_file, BaseModelReport,
    EvaluationRecipe, LoraContractReport, ToolCallingReport,
};

const LORA_EXPORT_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_EXPORT_PAYLOAD_JSON";
const LORA_EXPORT_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_EXPORT_PAYLOAD_PRETTY";
const DEFAULT_CORPUS_JSONL_FILENAMES: &[&str] = &[
    "tool-calling-corpus.jsonl",
    "burin-tool-calling-corpus.jsonl",
    "corpus.jsonl",
];

pub(super) async fn export_dataset(args: &ModelsLoraExportArgs) -> i32 {
    let report = match export_report(args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    render_embedded_lora_report(
        &report,
        LORA_EXPORT_PAYLOAD_ENV,
        LORA_EXPORT_PAYLOAD_PRETTY_ENV,
        "models/lora_export",
        args.json,
        "LoRA export",
    )
    .await
}

fn export_report(args: &ModelsLoraExportArgs) -> Result<LoraExportReport, String> {
    if !args.check && args.out.is_none() {
        return Err("harn models lora export requires --out unless --check is set".to_string());
    }
    let requested_tool_format = normalize_plan_tool_format(&args.tool_format)?;
    let resolved = harn_vm::llm_config::resolve_model_info(&args.base_model);
    let provider = resolve_lora_provider(args.provider.as_deref(), &resolved.provider);
    let catalog = harn_vm::llm_config::model_catalog_entry(&resolved.id);
    let capabilities = harn_vm::llm::capabilities::lookup(&provider, &resolved.id);
    let local_runtime =
        harn_vm::llm_config::provider_config(&provider).and_then(|provider| provider.local_runtime);
    let provider_supports_lora_launch = local_runtime
        .as_ref()
        .and_then(|runtime| runtime.lora_modules_arg.as_ref())
        .is_some();
    let lora_module_value_format = lora_modules_value_format(local_runtime.as_ref());
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
    let dataset_format = dataset_format_for_tool_format(&decision.effective).to_string();
    let modules_to_save = normalize_modules_to_save(&args.modules_to_save)?;
    let corpus_path = resolve_corpus_path(&args.corpus)?;
    let contract_id = lora_contract_id(
        &resolved.id,
        &provider,
        &decision.effective,
        &dataset_format,
        args.chat_template.as_deref(),
        &modules_to_save,
    )?;
    let target = ExportTarget {
        base_model: resolved.id.clone(),
        provider: provider.clone(),
        adapter_name: args.adapter_name.clone(),
        harn_tool_format: decision.effective.clone(),
        chat_template: args.chat_template.clone(),
        default_split: non_empty_or_default(&args.default_split, "train"),
        default_license: non_empty_or_default(&args.default_license, "unknown"),
        contract_id: contract_id.clone(),
        metadata: parse_target_metadata(&args.target_metadata)?,
    };
    let contract = lora_contract_report(
        contract_id.clone(),
        &target.base_model,
        &target.provider,
        &target.harn_tool_format,
        &dataset_format,
        target.chat_template.clone(),
        &modules_to_save,
    );
    let mut writer = if args.check {
        None
    } else {
        Some(create_jsonl_writer(
            args.out
                .as_deref()
                .expect("--out is required unless --check is set"),
        )?)
    };
    let mut stats = ExportStats::default();
    let regexes = ExportRegexes::new();
    let file = File::open(&corpus_path)
        .map_err(|error| format!("failed to open corpus {}: {error}", corpus_path.display()))?;
    let reader = BufReader::new(file);
    let mut errors = Vec::new();

    for (line_index, line) in reader.lines().enumerate() {
        let line_number = line_index + 1;
        let line = line.map_err(|error| {
            format!(
                "failed to read {}:{line_number}: {error}",
                corpus_path.display()
            )
        })?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        stats.records += 1;
        let value = match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(error) => {
                stats.skipped += 1;
                errors.push(format!("{line_number}: invalid JSON row: {error}"));
                continue;
            }
        };
        let Some(record) = value.as_object() else {
            stats.skipped += 1;
            errors.push(format!("{line_number}: row is not a JSON object"));
            continue;
        };
        let has_training_messages = record
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| !messages.is_empty());
        if !has_training_messages {
            stats.skipped += 1;
            continue;
        }
        let converted = if dataset_format == "messages_with_tool_calls" {
            convert_structured_record(record, &target, &regexes)
        } else {
            convert_text_records(record, &target, &dataset_format, &regexes)
        };
        let converted = match converted {
            Ok(converted) => converted,
            Err(error) => {
                stats.skipped += 1;
                errors.push(format!("{}: {error}", record_id(record, line_number)));
                continue;
            }
        };
        if converted.rows.is_empty() {
            stats.skipped += 1;
            continue;
        }
        stats.emitted += converted.rows.len() as u64;
        stats.tool_calls += converted.tool_calls;
        stats.tool_results += converted.tool_results;
        if let Some(writer) = writer.as_mut() {
            for row in converted.rows {
                serde_json::to_writer(&mut *writer, &row)
                    .map_err(|error| format!("failed to encode export row: {error}"))?;
                writer
                    .write_all(b"\n")
                    .map_err(|error| format!("failed to write export row: {error}"))?;
            }
        }
    }
    if let Some(writer) = writer.as_mut() {
        writer
            .flush()
            .map_err(|error| format!("failed to flush exported dataset: {error}"))?;
    }

    let output_path = args.out.as_ref().map(|path| path.display().to_string());
    let output_sha256 = if args.check {
        None
    } else {
        args.out.as_deref().map(sha256_file).transpose()?
    };
    let serving = export_serving_report(
        &target,
        &dataset_format,
        provider_supports_lora_launch,
        &lora_module_value_format,
    );
    let request_model = target
        .adapter_name
        .clone()
        .unwrap_or_else(|| "ADAPTER_MODEL".to_string());
    let eval_dataset = corpus_path.display().to_string();
    let promotion = lora_evaluation_recipe(
        &contract_id,
        &target.base_model,
        &target.provider,
        &request_model,
        &target.harn_tool_format,
        &eval_dataset,
        vec![
            "harn".to_string(),
            "eval".to_string(),
            "tool-calls".to_string(),
            "--planner".to_string(),
            request_model.clone(),
            "--tool-format".to_string(),
            target.harn_tool_format.clone(),
            "--dataset".to_string(),
            eval_dataset.clone(),
        ],
    );
    let manifest_path = if let Some(path) = args.manifest.as_deref() {
        write_export_manifest(
            path,
            ExportManifestWrite {
                input_path: &corpus_path,
                output_path: args.out.as_deref().filter(|_| !args.check),
                output_sha256: output_sha256.as_deref(),
                stats: &stats,
                target: &target,
                contract: &contract,
                serving: &serving,
                promotion: &promotion,
                errors: &errors,
            },
        )?;
        Some(path.display().to_string())
    } else {
        None
    };
    let mut warnings = Vec::new();
    if let Some(correction) = &decision.correction {
        warnings.push(correction.clone());
    }
    if args.check {
        warnings.push("check mode: validated conversion without writing JSONL rows".to_string());
    }
    if stats.skipped > 0 && errors.is_empty() {
        warnings.push(format!(
            "skipped {} record(s) with no exportable assistant tool-call turns",
            stats.skipped
        ));
    }
    if !errors.is_empty() {
        warnings.push(format!(
            "{} record(s) could not be exported; see errors for the first failures",
            errors.len()
        ));
    }
    let ok = errors.is_empty() && stats.emitted > 0;
    Ok(LoraExportReport {
        ok,
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
        request: ExportRequest {
            requested_tool_format,
            effective_tool_format: decision.effective,
            tool_format_correction: decision.correction,
            dataset_format,
            corpus: corpus_path.display().to_string(),
            check: args.check,
            default_split: target.default_split.clone(),
            default_license: target.default_license.clone(),
        },
        tool_calling: ToolCallingReport {
            native_tools: capabilities.native_tools,
            preferred_tool_format: capabilities.preferred_tool_format,
            text_tool_wire_format_supported: capabilities.text_tool_wire_format_supported,
            structured_output_mode: capabilities.structured_output_mode,
            recommended_endpoint: capabilities.recommended_endpoint,
        },
        target: ExportTargetReport {
            base_model: target.base_model,
            provider: target.provider,
            adapter_name: target.adapter_name,
            harn_tool_format: target.harn_tool_format,
            chat_template: target.chat_template,
            contract_id,
            metadata: target.metadata,
        },
        contract,
        serving,
        promotion,
        output: ExportOutput {
            path: output_path,
            sha256: output_sha256,
            manifest_path,
        },
        stats,
        warnings,
        errors,
    })
}

pub(super) fn resolve_corpus_path(raw: &str) -> Result<PathBuf, String> {
    let expanded = expand_home(raw);
    let path = PathBuf::from(expanded);
    let resolved = if path.is_dir() {
        DEFAULT_CORPUS_JSONL_FILENAMES
            .iter()
            .map(|name| path.join(name))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| path.join(DEFAULT_CORPUS_JSONL_FILENAMES[0]))
    } else {
        path
    };
    if !resolved.is_file() {
        return Err(format!("corpus JSONL not found: {}", resolved.display()));
    }
    Ok(resolved)
}

fn create_jsonl_writer(path: &Path) -> Result<BufWriter<File>, String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let file = File::create(path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
    Ok(BufWriter::new(file))
}

pub(super) struct ExportRegexes {
    available_tools: Regex,
    blank_lines: Regex,
    declare_function: Regex,
    result_end: Regex,
    result_start: Regex,
    pub(super) tool_block: Regex,
    tool_bullet: Regex,
    pub(super) tool_name: Regex,
}

impl ExportRegexes {
    pub(super) fn new() -> Self {
        Self {
            available_tools: Regex::new(r"(?im)^Available tools:\s*(.+)$")
                .expect("available-tools regex"),
            blank_lines: Regex::new(r"\n{3,}").expect("blank-lines regex"),
            declare_function: Regex::new(r"\bdeclare\s+function\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(")
                .expect("declare-function regex"),
            result_end: Regex::new(r"(?m)\[end of (?P<label>[^\]\n]+?) result\]\s*")
                .expect("tool-result-end regex"),
            result_start: Regex::new(r"(?m)\[result of (?P<label>[^\]\n]+)\]\s*")
                .expect("tool-result-start regex"),
            tool_block: Regex::new(r"(?s)<tool_call>\s*(.*?)\s*</tool_call>")
                .expect("tool-block regex"),
            tool_bullet: Regex::new(r"(?m)^\s*-\s*([A-Za-z_][A-Za-z0-9_]*)\b")
                .expect("tool-bullet regex"),
            tool_name: Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("tool-name regex"),
        }
    }
}

#[derive(Clone)]
struct ExportTarget {
    base_model: String,
    provider: String,
    adapter_name: Option<String>,
    harn_tool_format: String,
    chat_template: Option<String>,
    default_split: String,
    default_license: String,
    contract_id: String,
    metadata: BTreeMap<String, String>,
}

pub(super) struct ParsedToolCall {
    pub(super) name: String,
    arguments: Value,
}

struct ConvertedExport {
    rows: Vec<Value>,
    tool_calls: u64,
    tool_results: u64,
}

fn convert_structured_record(
    record: &Map<String, Value>,
    target: &ExportTarget,
    regexes: &ExportRegexes,
) -> Result<ConvertedExport, String> {
    let messages = record_messages(record)?;
    let system_text = messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("system"))
        .filter_map(|message| message.get("content").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let mut available_tools = available_tool_names(&system_text, regexes);
    let mut calls_by_tool: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut structured_messages = Vec::new();
    let mut pending_tool_calls: VecDeque<Value> = VecDeque::new();
    let mut tool_call_count = 0_u64;
    let mut tool_result_count = 0_u64;

    for (index, raw_message) in messages.iter().enumerate() {
        let role = raw_message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("");
        let content = raw_message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("");
        match role {
            "assistant" => {
                let parsed_calls = parse_json_tool_blocks(content, regexes)?;
                let assistant_content =
                    normalize_blank_lines(&regexes.tool_block.replace_all(content, "\n"), regexes);
                let mut message = Map::new();
                message.insert("role".to_string(), Value::String("assistant".to_string()));
                message.insert("content".to_string(), Value::String(assistant_content));
                if !parsed_calls.is_empty() {
                    let tool_calls = parsed_calls
                        .into_iter()
                        .enumerate()
                        .map(|(call_index, call)| {
                            available_tools.insert(call.name.clone());
                            calls_by_tool
                                .entry(call.name.clone())
                                .or_default()
                                .push(call.arguments.clone());
                            json!({
                                "id": format!("call_{index}_{}", call_index + 1),
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": call.arguments,
                                },
                            })
                        })
                        .collect::<Vec<_>>();
                    tool_call_count += tool_calls.len() as u64;
                    pending_tool_calls = tool_calls.iter().cloned().collect();
                    message.insert("tool_calls".to_string(), Value::Array(tool_calls));
                }
                structured_messages.push(Value::Object(message));
            }
            "user" => {
                if let Some(tool_results) =
                    structured_tool_result_messages(content, &mut pending_tool_calls, regexes)
                {
                    tool_result_count += tool_results.len() as u64;
                    structured_messages.extend(tool_results);
                } else {
                    pending_tool_calls.clear();
                    structured_messages.push(normalized_message(role, content));
                }
            }
            "system" => structured_messages.push(normalized_message(role, content)),
            _ => {}
        }
    }

    let tools = tool_schemas(&available_tools, &calls_by_tool);
    if tools.is_empty() {
        return Err("record exposes no tools".to_string());
    }
    let row = json!({
        "id": record_string(record, "id"),
        "eval_name": record_string(record, "eval_name"),
        "language": record_string(record, "language"),
        "task_type": record_string(record, "task_type"),
        "messages": structured_messages,
        "tools": tools,
        "metadata": export_metadata(
            record,
            target,
            "hf_trl_tool_calls_v1",
            "messages_with_tool_calls",
            &tools,
            &system_text,
        ),
    });
    Ok(ConvertedExport {
        rows: vec![row],
        tool_calls: tool_call_count,
        tool_results: tool_result_count,
    })
}

fn convert_text_records(
    record: &Map<String, Value>,
    target: &ExportTarget,
    dataset_format: &str,
    regexes: &ExportRegexes,
) -> Result<ConvertedExport, String> {
    let messages = record_messages(record)?;
    let system_text = messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("system"))
        .filter_map(|message| message.get("content").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let mut available_tools = available_tool_names(&system_text, regexes);
    let mut calls_by_tool: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut context = Vec::new();
    let mut rows = Vec::new();
    let mut tool_call_count = 0_u64;
    let source_tool_format = source_tool_format(record);

    for (index, raw_message) in messages.iter().enumerate() {
        let role = raw_message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("");
        let content = raw_message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !matches!(role, "system" | "user" | "assistant") {
            continue;
        }
        if role == "assistant" {
            let block_count = regexes.tool_block.find_iter(content).count();
            if block_count > 0 {
                if dataset_format == "harn_text_tool_calls_json_fences" {
                    let parsed_calls = parse_json_tool_blocks(content, regexes)?;
                    for call in parsed_calls {
                        available_tools.insert(call.name.clone());
                        calls_by_tool
                            .entry(call.name)
                            .or_default()
                            .push(call.arguments);
                        tool_call_count += 1;
                    }
                } else if source_tool_format != "text" {
                    return Err(format!(
                        "cannot export source tool_format={source_tool_format} as `{dataset_format}` without a text/heredoc source record"
                    ));
                } else {
                    tool_call_count += block_count as u64;
                }
                let tools = tool_schemas(&available_tools, &calls_by_tool);
                rows.push(json!({
                    "id": format!("{}#turn-{index}", record_id(record, index + 1)),
                    "source_id": record_string(record, "id"),
                    "eval_name": record_string(record, "eval_name"),
                    "language": record_string(record, "language"),
                    "task_type": record_string(record, "task_type"),
                    "messages": context,
                    "tools": tools,
                    "assistant_tool_text": content,
                    "metadata": export_metadata(
                        record,
                        target,
                        "harn_text_tool_calls_v1",
                        dataset_format,
                        &tools,
                        &system_text,
                    ),
                }));
            }
        }
        context.push(normalized_message(role, content));
    }
    Ok(ConvertedExport {
        rows,
        tool_calls: tool_call_count,
        tool_results: 0,
    })
}

pub(super) fn record_messages(
    record: &Map<String, Value>,
) -> Result<Vec<Map<String, Value>>, String> {
    let Some(messages) = record.get("messages").and_then(Value::as_array) else {
        return Err("record has no messages array".to_string());
    };
    if messages.is_empty() {
        return Err("record has no messages".to_string());
    }
    Ok(messages
        .iter()
        .filter_map(|message| message.as_object().cloned())
        .collect())
}

fn normalized_message(role: &str, content: &str) -> Value {
    json!({
        "role": role,
        "content": content,
    })
}

struct ToolResultBlock {
    name: String,
    content: String,
}

fn structured_tool_result_messages(
    content: &str,
    pending_tool_calls: &mut VecDeque<Value>,
    regexes: &ExportRegexes,
) -> Option<Vec<Value>> {
    let blocks = tool_result_blocks(content, regexes)?;
    if blocks.is_empty() {
        return None;
    }
    let mut remaining_tool_calls = pending_tool_calls.clone();
    let mut messages = Vec::new();
    for block in blocks {
        let next_call = remaining_tool_calls.pop_front()?;
        let call_name = next_call
            .get("function")
            .and_then(Value::as_object)
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)?;
        if call_name != block.name {
            return None;
        }
        let tool_call_id = next_call.get("id").and_then(Value::as_str).unwrap_or("");
        messages.push(json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "name": block.name,
            "content": block.content,
        }));
    }
    *pending_tool_calls = remaining_tool_calls;
    Some(messages)
}

fn tool_result_blocks(content: &str, regexes: &ExportRegexes) -> Option<Vec<ToolResultBlock>> {
    let text = content.trim();
    if text.is_empty() {
        return None;
    }
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while cursor < text.len() {
        cursor = skip_ascii_whitespace(text, cursor);
        if cursor >= text.len() {
            break;
        }
        let start = regexes.result_start.captures_at(text, cursor)?;
        let whole = start.get(0)?;
        if whole.start() != cursor {
            return None;
        }
        let name = tool_name_from_result_label(start.name("label")?.as_str(), regexes)?;
        let body_start = whole.end();
        let next_start = regexes.result_start.find_at(text, body_start);
        let end = regexes.result_end.find_at(text, body_start);
        let (body_end, next_cursor) = match (end, next_start) {
            (Some(end), Some(next_start)) if end.start() < next_start.start() => {
                (end.start(), end.end())
            }
            (Some(end), None) => (end.start(), end.end()),
            (_, Some(next_start)) => (next_start.start(), next_start.start()),
            (None, None) => (text.len(), text.len()),
        };
        blocks.push(ToolResultBlock {
            name,
            content: normalize_blank_lines(&text[body_start..body_end], regexes),
        });
        cursor = next_cursor;
    }
    Some(blocks)
}

fn tool_name_from_result_label(label: &str, regexes: &ExportRegexes) -> Option<String> {
    let name = label.split_whitespace().next()?;
    if regexes.tool_name.is_match(name) {
        return Some(name.to_string());
    }
    None
}

fn skip_ascii_whitespace(text: &str, mut cursor: usize) -> usize {
    while cursor < text.len() {
        let byte = text.as_bytes()[cursor];
        if !byte.is_ascii_whitespace() {
            break;
        }
        cursor += 1;
    }
    cursor
}

fn parse_json_tool_blocks(
    content: &str,
    regexes: &ExportRegexes,
) -> Result<Vec<ParsedToolCall>, String> {
    let mut calls = Vec::new();
    for captures in regexes.tool_block.captures_iter(content) {
        let body = captures.get(1).map(|match_| match_.as_str()).unwrap_or("");
        calls.extend(parse_json_tool_body(body)?);
    }
    Ok(calls)
}

pub(super) fn parse_json_tool_body(body: &str) -> Result<Vec<ParsedToolCall>, String> {
    let body = body.trim();
    if body.is_empty() {
        return Err("empty <tool_call> body".to_string());
    }
    if !body.starts_with('{') && !body.starts_with('[') {
        return Err(
            "structured/json export only accepts JSON object <tool_call> bodies".to_string(),
        );
    }
    let value = serde_json::from_str::<Value>(body)
        .map_err(|error| format!("invalid JSON <tool_call> body: {error}"))?;
    let items = match value {
        Value::Array(items) => items,
        item => vec![item],
    };
    let mut calls = Vec::new();
    for item in items {
        let Some(object) = item.as_object() else {
            return Err("tool call body must be an object or list of objects".to_string());
        };
        let function = object.get("function").and_then(Value::as_object);
        let name = object
            .get("name")
            .or_else(|| object.get("tool_name"))
            .and_then(Value::as_str)
            .or_else(|| {
                function
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
            })
            .ok_or_else(|| "tool call is missing a string name".to_string())?;
        let arguments = object
            .get("arguments")
            .or_else(|| object.get("parameters"))
            .or_else(|| object.get("args"))
            .cloned()
            .or_else(|| {
                function
                    .and_then(|function| function.get("arguments"))
                    .cloned()
            })
            .unwrap_or_else(|| json!({}));
        let arguments = match arguments {
            Value::String(text) => serde_json::from_str::<Value>(&text)
                .map_err(|error| format!("arguments string for {name} is not JSON: {error}"))?,
            value => value,
        };
        if !arguments.is_object() {
            return Err(format!("arguments for {name} must be an object"));
        }
        calls.push(ParsedToolCall {
            name: name.to_string(),
            arguments,
        });
    }
    Ok(calls)
}

pub(super) fn available_tool_names(system_text: &str, regexes: &ExportRegexes) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for captures in regexes.declare_function.captures_iter(system_text) {
        if let Some(name) = captures.get(1) {
            names.insert(name.as_str().to_string());
        }
    }
    for captures in regexes.available_tools.captures_iter(system_text) {
        let Some(raw) = captures.get(1) else {
            continue;
        };
        for part in raw
            .as_str()
            .split(|ch: char| ch == ',' || ch.is_whitespace())
        {
            let name = part.trim();
            if regexes.tool_name.is_match(name) {
                names.insert(name.to_string());
            }
        }
    }
    for captures in regexes.tool_bullet.captures_iter(system_text) {
        if let Some(name) = captures.get(1) {
            names.insert(name.as_str().to_string());
        }
    }
    names
}

fn tool_schemas(names: &BTreeSet<String>, examples: &BTreeMap<String, Vec<Value>>) -> Vec<Value> {
    names
        .iter()
        .map(|name| tool_schema(name, examples.get(name).map(Vec::as_slice).unwrap_or(&[])))
        .collect()
}

fn tool_schema(name: &str, examples: &[Value]) -> Value {
    let mut properties = Map::new();
    let mut required: Option<BTreeSet<String>> = None;
    for example in examples {
        let Some(args) = example.as_object() else {
            continue;
        };
        let keys = args.keys().cloned().collect::<BTreeSet<_>>();
        required = Some(match required {
            Some(existing) => existing.intersection(&keys).cloned().collect(),
            None => keys,
        });
        for (key, value) in args {
            properties
                .entry(key.clone())
                .or_insert_with(|| json_schema_type(value));
        }
    }
    let mut parameters = Map::new();
    parameters.insert("type".to_string(), Value::String("object".to_string()));
    parameters.insert("properties".to_string(), Value::Object(properties));
    parameters.insert("additionalProperties".to_string(), Value::Bool(true));
    if let Some(required) = required.filter(|required| !required.is_empty()) {
        parameters.insert(
            "required".to_string(),
            Value::Array(required.into_iter().map(Value::String).collect()),
        );
    }
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": format!("Harn agent tool `{name}`."),
            "parameters": parameters,
        },
    })
}

fn json_schema_type(value: &Value) -> Value {
    let type_name = match value {
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        _ => "string",
    };
    json!({ "type": type_name })
}

fn export_metadata(
    record: &Map<String, Value>,
    target: &ExportTarget,
    exporter: &str,
    dataset_format: &str,
    tools: &[Value],
    system_text: &str,
) -> Value {
    let mut metadata = record
        .get("metadata")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    metadata.insert(
        "lora_exporter".to_string(),
        Value::String(exporter.to_string()),
    );
    metadata.insert(
        "dataset_format".to_string(),
        Value::String(dataset_format.to_string()),
    );
    metadata.insert(
        "source_tool_format".to_string(),
        Value::String(source_tool_format(record)),
    );
    metadata.insert(
        "source_record_id".to_string(),
        Value::String(first_record_string(
            record,
            &metadata,
            &["source_record_id", "source_id", "id"],
        )),
    );
    metadata.insert(
        "source_transcript_id".to_string(),
        Value::String(first_record_string(
            record,
            &metadata,
            &[
                "source_transcript_id",
                "transcript_id",
                "conversation_id",
                "session_id",
                "id",
            ],
        )),
    );
    metadata.insert(
        "teacher_model".to_string(),
        Value::String(first_record_string(
            record,
            &metadata,
            &["teacher_model", "model", "source_model"],
        )),
    );
    metadata.insert(
        "teacher_provider".to_string(),
        Value::String(first_record_string(
            record,
            &metadata,
            &["teacher_provider", "provider", "source_provider"],
        )),
    );
    metadata.insert(
        "target_base_model".to_string(),
        Value::String(target.base_model.clone()),
    );
    metadata.insert(
        "target_tool_format".to_string(),
        Value::String(target.harn_tool_format.clone()),
    );
    metadata.insert(
        "tool_schema_hash".to_string(),
        Value::String(canonical_json_sha256(&Value::Array(tools.to_vec()))),
    );
    metadata.insert(
        "prompt_template_hash".to_string(),
        Value::String(prompt_template_hash(target, dataset_format, system_text)),
    );
    metadata.insert(
        "split".to_string(),
        Value::String(string_or_default(
            first_record_string(record, &metadata, &["split"]),
            &target.default_split,
        )),
    );
    metadata.insert(
        "license".to_string(),
        Value::String(string_or_default(
            first_record_string(record, &metadata, &["license"]),
            &target.default_license,
        )),
    );
    metadata.insert("lora_target".to_string(), export_target_value(target));
    metadata.insert(
        "lora_contract_id".to_string(),
        Value::String(target.contract_id.clone()),
    );
    Value::Object(metadata)
}

fn first_record_string(
    record: &Map<String, Value>,
    metadata: &Map<String, Value>,
    keys: &[&str],
) -> String {
    for key in keys {
        if let Some(value) = metadata
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            return value.trim().to_string();
        }
        if let Some(value) = record
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            return value.trim().to_string();
        }
    }
    String::new()
}

fn string_or_default(value: String, default: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

fn non_empty_or_default(value: &str, default: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

fn prompt_template_hash(target: &ExportTarget, dataset_format: &str, system_text: &str) -> String {
    canonical_json_sha256(&json!({
        "chat_template": target.chat_template.as_deref().unwrap_or(""),
        "dataset_format": dataset_format,
        "harn_tool_format": target.harn_tool_format.as_str(),
        "system": system_text,
    }))
}

fn canonical_json_sha256(value: &Value) -> String {
    let canonical = canonical_json_value(value);
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn canonical_json_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(canonical_json_value).collect()),
        Value::Object(object) => {
            let mut sorted = Map::new();
            let mut entries = object.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| *key);
            for (key, value) in entries {
                sorted.insert(key.clone(), canonical_json_value(value));
            }
            Value::Object(sorted)
        }
        value => value.clone(),
    }
}

fn export_target_value(target: &ExportTarget) -> Value {
    let mut object = Map::new();
    object.insert(
        "base_model".to_string(),
        Value::String(target.base_model.clone()),
    );
    object.insert(
        "provider".to_string(),
        Value::String(target.provider.clone()),
    );
    object.insert(
        "harn_tool_format".to_string(),
        Value::String(target.harn_tool_format.clone()),
    );
    if let Some(adapter_name) = &target.adapter_name {
        object.insert(
            "adapter_name".to_string(),
            Value::String(adapter_name.clone()),
        );
    }
    if let Some(chat_template) = &target.chat_template {
        object.insert(
            "chat_template".to_string(),
            Value::String(chat_template.clone()),
        );
    }
    object.insert(
        "contract_id".to_string(),
        Value::String(target.contract_id.clone()),
    );
    if !target.metadata.is_empty() {
        object.insert(
            "metadata".to_string(),
            serde_json::to_value(&target.metadata).unwrap_or_else(|_| json!({})),
        );
    }
    Value::Object(object)
}

fn export_serving_report(
    target: &ExportTarget,
    dataset_format: &str,
    provider_supports_lora_launch: bool,
    lora_module_value_format: &str,
) -> ExportServingReport {
    ExportServingReport {
        request_model: target.adapter_name.clone(),
        adapter_name: target.adapter_name.clone(),
        base_model: target.base_model.clone(),
        provider: target.provider.clone(),
        adapter_binding: lora_adapter_binding(provider_supports_lora_launch).to_string(),
        lora_module_value_format: lora_module_value_format.to_string(),
        tool_format: target.harn_tool_format.clone(),
        dataset_format: dataset_format.to_string(),
        contract_id: target.contract_id.clone(),
    }
}

fn record_string(record: &Map<String, Value>, key: &str) -> String {
    record
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(super) fn record_id(record: &Map<String, Value>, fallback_line: usize) -> String {
    record
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("line-{fallback_line}"))
}

pub(super) fn source_tool_format(record: &Map<String, Value>) -> String {
    record
        .get("metadata")
        .and_then(Value::as_object)
        .and_then(|metadata| metadata.get("tool_format"))
        .and_then(Value::as_str)
        .filter(|format| !format.is_empty())
        .unwrap_or("json")
        .to_string()
}

fn normalize_blank_lines(value: &str, regexes: &ExportRegexes) -> String {
    regexes
        .blank_lines
        .replace_all(value, "\n\n")
        .trim()
        .to_string()
}

struct ExportManifestWrite<'a> {
    input_path: &'a Path,
    output_path: Option<&'a Path>,
    output_sha256: Option<&'a str>,
    stats: &'a ExportStats,
    target: &'a ExportTarget,
    contract: &'a LoraContractReport,
    serving: &'a ExportServingReport,
    promotion: &'a EvaluationRecipe,
    errors: &'a [String],
}

fn write_export_manifest(path: &Path, manifest: ExportManifestWrite<'_>) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let output = manifest.output_path.map(|path| {
        json!({
            "path": path.display().to_string(),
            "sha256": manifest.output_sha256.unwrap_or(""),
        })
    });
    let manifest = json!({
        "exporter": "harn_models_lora_export_v1",
        "dataset_format": manifest.serving.dataset_format.as_str(),
        "input": {
            "path": manifest.input_path.display().to_string(),
            "sha256": sha256_file(manifest.input_path).unwrap_or_default(),
        },
        "output": output,
        "stats": manifest.stats,
        "target": export_target_value(manifest.target),
        "contract": manifest.contract,
        "provenance": {
            "required_example_metadata": &manifest.contract.training_contract.required_example_metadata,
            "default_split": manifest.target.default_split.as_str(),
            "default_license": manifest.target.default_license.as_str(),
        },
        "serving": manifest.serving,
        "promotion": manifest.promotion,
        "errors": manifest.errors,
    });
    std::fs::write(
        path,
        serde_json::to_string_pretty(&manifest)
            .map_err(|error| format!("failed to render manifest JSON: {error}"))?
            + "\n",
    )
    .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

#[derive(Debug, Serialize)]
struct LoraExportReport {
    ok: bool,
    base: BaseModelReport,
    request: ExportRequest,
    tool_calling: ToolCallingReport,
    target: ExportTargetReport,
    contract: LoraContractReport,
    serving: ExportServingReport,
    promotion: EvaluationRecipe,
    output: ExportOutput,
    stats: ExportStats,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ExportRequest {
    requested_tool_format: String,
    effective_tool_format: String,
    tool_format_correction: Option<String>,
    dataset_format: String,
    corpus: String,
    check: bool,
    default_split: String,
    default_license: String,
}

#[derive(Debug, Serialize)]
struct ExportTargetReport {
    base_model: String,
    provider: String,
    adapter_name: Option<String>,
    harn_tool_format: String,
    chat_template: Option<String>,
    contract_id: String,
    metadata: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
struct ExportServingReport {
    request_model: Option<String>,
    adapter_name: Option<String>,
    base_model: String,
    provider: String,
    adapter_binding: String,
    lora_module_value_format: String,
    tool_format: String,
    dataset_format: String,
    contract_id: String,
}

#[derive(Debug, Serialize)]
struct ExportOutput {
    path: Option<String>,
    sha256: Option<String>,
    manifest_path: Option<String>,
}

#[derive(Default, Debug, Serialize)]
struct ExportStats {
    records: u64,
    emitted: u64,
    skipped: u64,
    tool_calls: u64,
    tool_results: u64,
}
