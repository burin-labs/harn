use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufRead as _, BufReader};
use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::cli::{ModelsLoraBehaviorStrataPolicy, ModelsLoraPreflightArgs};

use super::behavior::{
    behavior_strata_report, record_behavior_classes, BehaviorStrataPolicy, BehaviorStrataReport,
    BehaviorStrataStatus,
};
use super::export::{
    available_tool_names, parse_json_tool_body, record_id, record_messages, resolve_corpus_path,
    source_tool_format, ExportRegexes,
};
use super::{
    normalize_plan_tool_format, render_embedded_report, resolve_lora_provider,
    source_tool_format_required_for_target, BaseModelReport, ToolCallingReport,
};

const LORA_PREFLIGHT_PAYLOAD_ENV: &str = "HARN_MODELS_LORA_PREFLIGHT_PAYLOAD_JSON";
const LORA_PREFLIGHT_PAYLOAD_PRETTY_ENV: &str = "HARN_MODELS_LORA_PREFLIGHT_PAYLOAD_PRETTY";
const DEFAULT_MIN_FIT_RATIO: f64 = 0.90;

pub(super) async fn preflight(args: &ModelsLoraPreflightArgs) -> i32 {
    let report = match preflight_report(args) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let check = report.request.check;
    let exit_code = render_embedded_report(
        &report,
        LORA_PREFLIGHT_PAYLOAD_ENV,
        LORA_PREFLIGHT_PAYLOAD_PRETTY_ENV,
        "models/lora_preflight",
        args.json,
        "LoRA preflight",
    )
    .await;
    if check {
        exit_code
    } else {
        0
    }
}

fn preflight_report(args: &ModelsLoraPreflightArgs) -> Result<LoraPreflightReport, String> {
    let expected_source_format = normalize_source_tool_format(&args.source_tool_format)?;
    let requested_tool_format = normalize_plan_tool_format(&args.tool_format)?;
    validate_ratio("--min-tool-call-share", args.min_tool_call_share)?;
    if let Some(value) = args.min_fit_ratio {
        validate_ratio("--min-fit-ratio", value)?;
    }
    if args.hard_token_limit == 0 {
        return Err("--hard-token-limit must be greater than zero".to_string());
    }
    if matches!(args.max_seq_length, Some(0)) {
        return Err("--max-seq-length must be greater than zero".to_string());
    }

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
    let target_tool_format = decision.effective.clone();
    let required_source_format = source_tool_format_required_for_target(&target_tool_format);
    let config = args
        .config
        .as_deref()
        .map(read_training_config)
        .transpose()?
        .unwrap_or_default();
    let max_seq_length = args
        .max_seq_length
        .or(config.max_seq_length)
        .unwrap_or(args.hard_token_limit);
    let min_fit_ratio = args
        .min_fit_ratio
        .or(config.min_fit_ratio)
        .unwrap_or(DEFAULT_MIN_FIT_RATIO);
    validate_ratio("effective min_fit_ratio", min_fit_ratio)?;

    let corpus_path = resolve_corpus_path(&args.corpus)?;
    let regexes = ExportRegexes::new();
    let mut raw_records = 0_u64;
    let mut skipped_records = Vec::new();
    let mut load_errors = Vec::new();
    let mut examples = Vec::new();
    let file = File::open(&corpus_path)
        .map_err(|error| format!("failed to open corpus {}: {error}", corpus_path.display()))?;
    for (line_index, line) in BufReader::new(file).lines().enumerate() {
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
        raw_records += 1;
        let value = match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(error) => {
                load_errors.push(format!("{line_number}: invalid JSON row: {error}"));
                continue;
            }
        };
        let Some(record) = value.as_object() else {
            load_errors.push(format!("{line_number}: row is not a JSON object"));
            continue;
        };
        let has_messages = record
            .get("messages")
            .and_then(Value::as_array)
            .is_some_and(|messages| !messages.is_empty());
        if !has_messages {
            skipped_records.push(format!(
                "{} (line {line_number}, no messages)",
                record_id(record, line_number)
            ));
            continue;
        }
        examples.push(analyze_record(
            line_number,
            record,
            &regexes,
            args.done_marker.as_deref(),
        )?);
    }

    let stats = preflight_stats(
        &examples,
        raw_records,
        max_seq_length,
        args.hard_token_limit,
        match args.behavior_strata_policy {
            ModelsLoraBehaviorStrataPolicy::Strict => BehaviorStrataPolicy::Strict,
            ModelsLoraBehaviorStrataPolicy::LegacyUnclassified => {
                BehaviorStrataPolicy::LegacyUnclassified
            }
        },
    );
    let breakdown = preflight_breakdown(&examples);
    let problem_examples = problem_examples(&examples);
    let tool_call_share = matching_tool_call_share(&stats.tool_calls, &expected_source_format);
    let export_ready_tool_call_share =
        matching_tool_call_share(&stats.tool_calls, required_source_format);
    let thresholds = PreflightThresholds {
        max_seq_length,
        min_fit_ratio,
        hard_token_limit: args.hard_token_limit,
        min_records: args.min_records,
        expected_source_tool_format: expected_source_format.clone(),
        required_export_source_tool_format: required_source_format.to_string(),
        min_tool_call_share: args.min_tool_call_share,
        done_marker: args.done_marker.clone(),
    };
    let mut errors = load_errors;
    if stats.trainable_records < args.min_records {
        errors.push(format!(
            "records {} below required floor {}",
            stats.trainable_records, args.min_records
        ));
    }
    if stats.fit_ratio < min_fit_ratio {
        errors.push(format!(
            "fit ratio {:.1}% below required floor {:.1}% for max_seq_length={max_seq_length}",
            stats.fit_ratio * 100.0,
            min_fit_ratio * 100.0
        ));
    }
    if tool_call_share < args.min_tool_call_share {
        errors.push(format!(
            "{expected_source_format} tool-call share {:.1}% below required floor {:.1}%",
            tool_call_share * 100.0,
            args.min_tool_call_share * 100.0
        ));
    }
    if required_source_format != expected_source_format
        && export_ready_tool_call_share < args.min_tool_call_share
    {
        errors.push(format!(
            "{target_tool_format} target requires {required_source_format} source tool calls, but export-ready share {:.1}% is below required floor {:.1}%",
            export_ready_tool_call_share * 100.0,
            args.min_tool_call_share * 100.0
        ));
    }
    if stats.hard_overflow_records > 0 {
        errors.push(format!(
            "{} record(s) exceed hard token estimate {}",
            stats.hard_overflow_records, args.hard_token_limit
        ));
    }
    if args.done_marker.is_some() && stats.missing_done_marker_records > 0 {
        let ids = summarize_problem_ids(&examples, |example| example.missing_done_marker);
        errors.push(format!(
            "{} record(s) missing required done marker: {ids}",
            stats.missing_done_marker_records
        ));
    }
    if stats.tool_calls.malformed_json_bodies > 0 || stats.tool_calls.unknown_tool_blocks > 0 {
        errors.push(format!(
            "{} malformed JSON tool-call bodie(s) and {} unknown tool-call block(s)",
            stats.tool_calls.malformed_json_bodies, stats.tool_calls.unknown_tool_blocks
        ));
    }
    if stats.records_with_unrecognized_tools > 0 {
        let ids =
            summarize_problem_ids(&examples, |example| !example.unrecognized_tools.is_empty());
        errors.push(format!(
            "{} record(s) contain tool names not declared by the transcript system prompt: {ids}",
            stats.records_with_unrecognized_tools
        ));
    }
    if !stats.behavior_strata.missing_required.is_empty()
        && stats.behavior_strata.status != BehaviorStrataStatus::LegacyUnclassified
    {
        errors.push(format!(
            "missing required behavior strata: {}",
            stats.behavior_strata.missing_required.join(", ")
        ));
    }

    let mut warnings = Vec::new();
    if args.config.is_none() && args.max_seq_length.is_none() {
        warnings.push(format!(
            "no --config or --max-seq-length supplied; using --hard-token-limit {max_seq_length} as the sequence budget"
        ));
    }
    if !skipped_records.is_empty() {
        warnings.push(format!(
            "skipped {} record(s) with no messages",
            skipped_records.len()
        ));
    }
    if args.check {
        warnings.push("check mode: readiness failures exit non-zero".to_string());
    }
    if let Some(correction) = &decision.correction {
        warnings.push(correction.clone());
    }
    if stats.behavior_strata.status == BehaviorStrataStatus::LegacyUnclassified {
        warnings.push(
            "WARNING: legacy corpus has no declared behavior-strata metadata; coverage is unverified and all required classes remain missing. Review and tag the reported unclassified record ids before export or training."
                .to_string(),
        );
    }

    Ok(LoraPreflightReport {
        ok: errors.is_empty(),
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
        request: PreflightRequest {
            corpus: corpus_path.display().to_string(),
            config: args.config.as_ref().map(|path| path.display().to_string()),
            check: args.check,
            requested_tool_format,
            source_tool_format: expected_source_format,
            tool_format_correction: decision.correction,
            target_tool_format,
        },
        tool_calling: ToolCallingReport {
            native_tools: capabilities.native_tools,
            preferred_tool_format: capabilities.preferred_tool_format,
            text_tool_wire_format_supported: capabilities.text_tool_wire_format_supported,
            structured_output_mode: capabilities.structured_output_mode,
            recommended_endpoint: capabilities.recommended_endpoint,
        },
        config: TrainingConfigReport {
            path: args.config.as_ref().map(|path| path.display().to_string()),
            max_seq_length: config.max_seq_length,
            min_fit_ratio: config.min_fit_ratio,
        },
        thresholds,
        stats,
        breakdown,
        longest: longest_examples(&examples),
        problem_examples,
        skipped_records,
        warnings,
        errors,
    })
}

fn read_training_config(path: &Path) -> Result<TrainingConfig, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read config {}: {error}", path.display()))?;
    let mut config = TrainingConfig::default();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(value) = scalar_value(line, "max_seq_length") {
            config.max_seq_length = Some(value.parse::<u64>().map_err(|error| {
                format!("invalid max_seq_length in {}: {error}", path.display())
            })?);
        }
        if let Some(value) = scalar_value(line, "min_fit_ratio") {
            config.min_fit_ratio = Some(value.parse::<f64>().map_err(|error| {
                format!("invalid min_fit_ratio in {}: {error}", path.display())
            })?);
        }
    }
    Ok(config)
}

fn scalar_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let (name, value) = line.split_once(':')?;
    if name.trim() != key {
        return None;
    }
    Some(value.trim().trim_matches('"').trim_matches('\''))
}

fn analyze_record(
    line_number: usize,
    record: &Map<String, Value>,
    regexes: &ExportRegexes,
    done_marker: Option<&str>,
) -> Result<ExampleStats, String> {
    let messages = record_messages(record)?;
    let system_text = messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("system"))
        .filter_map(|message| message.get("content").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let declared_tools = available_tool_names(&system_text, regexes);
    let mut json_tool_calls = 0_u64;
    let mut text_tool_calls = 0_u64;
    let mut unknown_tool_blocks = 0_u64;
    let mut malformed_json_bodies = 0_u64;
    let mut unrecognized_tools = BTreeSet::new();
    let mut assistant_message_count = 0_u64;
    let mut last_assistant_content = "";
    let behavior_classes = record_behavior_classes(record)?;

    for message in &messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        assistant_message_count += 1;
        let content = message.get("content").and_then(Value::as_str).unwrap_or("");
        last_assistant_content = content;
        for captures in regexes.tool_block.captures_iter(content) {
            let body = captures.get(1).map(|match_| match_.as_str()).unwrap_or("");
            match parse_json_tool_body(body) {
                Ok(calls) => {
                    json_tool_calls += calls.len() as u64;
                    for call in calls {
                        if !declared_tools.is_empty() && !declared_tools.contains(&call.name) {
                            unrecognized_tools.insert(call.name);
                        }
                    }
                }
                Err(_) if body.trim_start().starts_with(['{', '[']) => {
                    malformed_json_bodies += 1;
                }
                Err(_) => {
                    if let Some(name) = text_tool_name(body, regexes) {
                        text_tool_calls += 1;
                        if !declared_tools.is_empty() && !declared_tools.contains(&name) {
                            unrecognized_tools.insert(name);
                        }
                    } else {
                        unknown_tool_blocks += 1;
                    }
                }
            }
        }
    }

    Ok(ExampleStats {
        line_number,
        record_id: record_id(record, line_number),
        eval_name: record
            .get("eval_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        language: record
            .get("language")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        task_type: record
            .get("task_type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        declared_tool_format: source_tool_format(record),
        message_count: messages.len() as u64,
        assistant_message_count,
        approx_tokens: estimate_tokens(&messages),
        json_tool_calls,
        text_tool_calls,
        unknown_tool_blocks,
        malformed_json_bodies,
        unrecognized_tools: unrecognized_tools.into_iter().collect(),
        behavior_classes: behavior_classes.into_iter().collect(),
        missing_done_marker: done_marker
            .is_some_and(|marker| !last_assistant_content.contains(marker)),
    })
}

fn text_tool_name(body: &str, regexes: &ExportRegexes) -> Option<String> {
    let body = body.trim_start();
    let name = body
        .split(|ch: char| !(ch == '_' || ch.is_ascii_alphanumeric()))
        .next()
        .unwrap_or("");
    if name.is_empty() || !regexes.tool_name.is_match(name) {
        return None;
    }
    let remainder = body[name.len()..].trim_start();
    if remainder.starts_with('(') {
        Some(name.to_string())
    } else {
        None
    }
}

fn estimate_tokens(messages: &[Map<String, Value>]) -> u64 {
    let mut total_chars = 0_u64;
    let mut message_count = 0_u64;
    for message in messages {
        if let Some(role) = message.get("role").and_then(Value::as_str) {
            total_chars += role.len() as u64;
        }
        if let Some(content) = message.get("content").and_then(Value::as_str) {
            total_chars += content.len() as u64;
        }
        message_count += 1;
    }
    total_chars.div_ceil(4) + (8 * message_count)
}

fn preflight_stats(
    examples: &[ExampleStats],
    raw_records: u64,
    max_seq_length: u64,
    hard_token_limit: u64,
    behavior_strata_policy: BehaviorStrataPolicy,
) -> PreflightStats {
    let trainable_records = examples.len() as u64;
    let fit_records = examples
        .iter()
        .filter(|example| example.approx_tokens <= max_seq_length)
        .count() as u64;
    let hard_overflow_records = examples
        .iter()
        .filter(|example| example.approx_tokens > hard_token_limit)
        .count() as u64;
    let missing_done_marker_records = examples
        .iter()
        .filter(|example| example.missing_done_marker)
        .count() as u64;
    let records_with_unrecognized_tools = examples
        .iter()
        .filter(|example| !example.unrecognized_tools.is_empty())
        .count() as u64;
    let mut tool_calls = ToolCallStats::default();
    let mut source_behavior_counts = BTreeMap::new();
    for example in examples {
        tool_calls.json_tool_calls += example.json_tool_calls;
        tool_calls.text_tool_calls += example.text_tool_calls;
        tool_calls.unknown_tool_blocks += example.unknown_tool_blocks;
        tool_calls.malformed_json_bodies += example.malformed_json_bodies;
        for class in &example.behavior_classes {
            *source_behavior_counts.entry(class.clone()).or_insert(0) += 1;
        }
    }
    let unclassified_record_ids = examples
        .iter()
        .filter(|example| example.behavior_classes.is_empty())
        .map(|example| example.record_id.clone())
        .collect();
    let behavior_strata = behavior_strata_report(
        source_behavior_counts.clone(),
        source_behavior_counts,
        behavior_strata_policy,
        unclassified_record_ids,
    );
    PreflightStats {
        raw_records,
        trainable_records,
        fit_records,
        fit_ratio: ratio(fit_records, trainable_records),
        hard_overflow_records,
        missing_done_marker_records,
        records_with_unrecognized_tools,
        tool_calls,
        behavior_strata,
    }
}

fn preflight_breakdown(examples: &[ExampleStats]) -> PreflightBreakdown {
    PreflightBreakdown {
        declared_tool_formats: count_by(examples, |example| &example.declared_tool_format),
        languages: count_by(examples, |example| &example.language),
        task_types: count_by(examples, |example| &example.task_type),
    }
}

fn count_by<'a>(
    examples: &'a [ExampleStats],
    key: impl Fn(&'a ExampleStats) -> &'a str,
) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for example in examples {
        let value = key(example);
        *counts
            .entry(if value.is_empty() { "(missing)" } else { value }.to_string())
            .or_insert(0) += 1;
    }
    counts
}

fn longest_examples(examples: &[ExampleStats]) -> Vec<LongestExample> {
    let mut examples = examples.to_vec();
    examples.sort_by_key(|example| std::cmp::Reverse(example.approx_tokens));
    examples
        .into_iter()
        .take(20)
        .map(|example| LongestExample {
            id: example.record_id,
            eval_name: example.eval_name,
            language: example.language,
            task_type: example.task_type,
            line_number: example.line_number as u64,
            approx_tokens: example.approx_tokens,
            message_count: example.message_count,
            assistant_message_count: example.assistant_message_count,
        })
        .collect()
}

fn problem_examples(examples: &[ExampleStats]) -> Vec<ProblemExample> {
    let mut problems = Vec::new();
    for example in examples {
        if example.missing_done_marker {
            problems.push(ProblemExample {
                id: example.record_id.clone(),
                line_number: example.line_number as u64,
                kind: "missing_done_marker".to_string(),
                detail: "last assistant message does not contain the required done marker"
                    .to_string(),
            });
        }
        if !example.unrecognized_tools.is_empty() {
            problems.push(ProblemExample {
                id: example.record_id.clone(),
                line_number: example.line_number as u64,
                kind: "unrecognized_tools".to_string(),
                detail: example.unrecognized_tools.join(", "),
            });
        }
        if example.malformed_json_bodies > 0 || example.unknown_tool_blocks > 0 {
            problems.push(ProblemExample {
                id: example.record_id.clone(),
                line_number: example.line_number as u64,
                kind: "malformed_tool_calls".to_string(),
                detail: format!(
                    "malformed_json={} unknown_blocks={}",
                    example.malformed_json_bodies, example.unknown_tool_blocks
                ),
            });
        }
    }
    problems
}

fn summarize_problem_ids(
    examples: &[ExampleStats],
    predicate: impl Fn(&ExampleStats) -> bool,
) -> String {
    let ids = examples
        .iter()
        .filter(|example| predicate(example))
        .take(8)
        .map(|example| example.record_id.clone())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        "(none)".to_string()
    } else {
        ids.join(", ")
    }
}

fn matching_tool_call_share(stats: &ToolCallStats, expected_source_format: &str) -> f64 {
    let total = stats.json_tool_calls
        + stats.text_tool_calls
        + stats.unknown_tool_blocks
        + stats.malformed_json_bodies;
    let matching = match expected_source_format {
        "json" => stats.json_tool_calls,
        "text" => stats.text_tool_calls,
        "auto" => stats.json_tool_calls + stats.text_tool_calls,
        _ => 0,
    };
    ratio(matching, total)
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        return 0.0;
    }
    numerator as f64 / denominator as f64
}

fn normalize_source_tool_format(raw: &str) -> Result<String, String> {
    let value = raw.trim().to_ascii_lowercase();
    match value.as_str() {
        "auto" | "json" | "text" => Ok(value),
        _ => Err(format!(
            "unsupported --source-tool-format `{raw}`; expected auto, json, or text"
        )),
    }
}

fn validate_ratio(name: &str, value: f64) -> Result<(), String> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(format!("{name} must be a finite ratio between 0 and 1"))
    }
}

#[derive(Default, Clone, Copy)]
struct TrainingConfig {
    max_seq_length: Option<u64>,
    min_fit_ratio: Option<f64>,
}

#[derive(Clone)]
struct ExampleStats {
    line_number: usize,
    record_id: String,
    eval_name: String,
    language: String,
    task_type: String,
    declared_tool_format: String,
    message_count: u64,
    assistant_message_count: u64,
    approx_tokens: u64,
    json_tool_calls: u64,
    text_tool_calls: u64,
    unknown_tool_blocks: u64,
    malformed_json_bodies: u64,
    unrecognized_tools: Vec<String>,
    behavior_classes: Vec<String>,
    missing_done_marker: bool,
}

#[derive(Debug, Serialize)]
struct LoraPreflightReport {
    ok: bool,
    base: BaseModelReport,
    request: PreflightRequest,
    tool_calling: ToolCallingReport,
    config: TrainingConfigReport,
    thresholds: PreflightThresholds,
    stats: PreflightStats,
    breakdown: PreflightBreakdown,
    longest: Vec<LongestExample>,
    problem_examples: Vec<ProblemExample>,
    skipped_records: Vec<String>,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PreflightRequest {
    corpus: String,
    config: Option<String>,
    check: bool,
    requested_tool_format: String,
    source_tool_format: String,
    tool_format_correction: Option<String>,
    target_tool_format: String,
}

#[derive(Debug, Serialize)]
struct TrainingConfigReport {
    path: Option<String>,
    max_seq_length: Option<u64>,
    min_fit_ratio: Option<f64>,
}

#[derive(Debug, Serialize)]
struct PreflightThresholds {
    max_seq_length: u64,
    min_fit_ratio: f64,
    hard_token_limit: u64,
    min_records: u64,
    expected_source_tool_format: String,
    required_export_source_tool_format: String,
    min_tool_call_share: f64,
    done_marker: Option<String>,
}

#[derive(Debug, Serialize)]
struct PreflightStats {
    raw_records: u64,
    trainable_records: u64,
    fit_records: u64,
    fit_ratio: f64,
    hard_overflow_records: u64,
    missing_done_marker_records: u64,
    records_with_unrecognized_tools: u64,
    tool_calls: ToolCallStats,
    behavior_strata: BehaviorStrataReport,
}

#[derive(Default, Debug, Serialize)]
struct ToolCallStats {
    json_tool_calls: u64,
    text_tool_calls: u64,
    unknown_tool_blocks: u64,
    malformed_json_bodies: u64,
}

#[derive(Debug, Serialize)]
struct PreflightBreakdown {
    declared_tool_formats: BTreeMap<String, u64>,
    languages: BTreeMap<String, u64>,
    task_types: BTreeMap<String, u64>,
}

#[derive(Debug, Serialize)]
struct LongestExample {
    id: String,
    eval_name: String,
    language: String,
    task_type: String,
    line_number: u64,
    approx_tokens: u64,
    message_count: u64,
    assistant_message_count: u64,
}

#[derive(Debug, Serialize)]
struct ProblemExample {
    id: String,
    line_number: u64,
    kind: String,
    detail: String,
}
