//! Provider-specific text-channel recovery dialects for the tagged grammar.
//!
//! Mistral `[TOOL_CALLS]` markers and DeepSeek DSML invokes are recognized
//! early by the tagged scanner so those models' chat-template emissions
//! become real calls instead of stray prose.

use std::collections::BTreeSet;

use super::super::syntax::{ident_length, parse_ts_call_from};
use super::super::TextToolParseResult;
use super::html_entities::decode_html_entities;
use super::{canonical_for_recovered_calls, prose_without_ranges};
use crate::llm::tools::collect_tool_schemas;
use crate::value::VmValue;

pub(super) fn parse_mistral_marker_calls(
    src: &str,
    tools_val: Option<&VmValue>,
) -> Option<TextToolParseResult> {
    const CALL_MARKER: &str = "[TOOL_CALLS]";
    const ARGS_MARKER: &str = "[ARGS]";

    if !src.contains(CALL_MARKER) {
        return None;
    }

    let known: BTreeSet<String> = collect_tool_schemas(tools_val, None)
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    let bytes = src.as_bytes();
    let mut cursor = 0usize;
    let mut calls = Vec::new();
    let mut errors = Vec::new();
    let mut ranges = Vec::<(usize, usize)>::new();

    while let Some(relative) = src[cursor..].find(CALL_MARKER) {
        let start = cursor + relative;
        let mut name_start = start + CALL_MARKER.len();
        while name_start < bytes.len() && bytes[name_start].is_ascii_whitespace() {
            name_start += 1;
        }
        if matches!(bytes.get(name_start), Some(b'[' | b'{')) {
            match parse_mistral_json_payload(&src[name_start..], &known, calls.len()) {
                Ok((mut parsed_calls, consumed)) => {
                    ranges.push((start, name_start + consumed));
                    calls.append(&mut parsed_calls);
                    cursor = name_start + consumed;
                }
                Err(message) => {
                    errors.push(message);
                    cursor = name_start.saturating_add(1);
                }
            }
            continue;
        }
        let Some(name_len) = ident_length(&bytes[name_start..]) else {
            errors.push("Mistral [TOOL_CALLS] marker was missing a tool name.".to_string());
            cursor = name_start.saturating_add(1);
            continue;
        };
        let name = &src[name_start..name_start + name_len];
        if !known.contains(name) {
            errors.push(format!(
                "Unknown tool '{name}' in Mistral [TOOL_CALLS] marker."
            ));
            cursor = name_start + name_len;
            continue;
        }
        let mut args_marker_start = name_start + name_len;
        while args_marker_start < bytes.len() && bytes[args_marker_start].is_ascii_whitespace() {
            args_marker_start += 1;
        }
        if !src[args_marker_start..].starts_with(ARGS_MARKER) {
            errors.push(format!(
                "Mistral [TOOL_CALLS] marker for `{name}` was missing [ARGS]."
            ));
            cursor = args_marker_start;
            continue;
        }
        let mut args_start = args_marker_start + ARGS_MARKER.len();
        while args_start < bytes.len() && bytes[args_start].is_ascii_whitespace() {
            args_start += 1;
        }
        let next_marker = src[args_start..]
            .find(CALL_MARKER)
            .map(|relative| args_start + relative)
            .unwrap_or(src.len());
        let raw_args = src[args_start..next_marker].trim();
        let synthetic = format!("{name}({raw_args})");
        match parse_ts_call_from(&synthetic, name.to_string()) {
            Ok((arguments, _)) => {
                calls.push(serde_json::json!({
                    "id": format!("tc_{}", calls.len()),
                    "name": name,
                    "arguments": arguments,
                }));
                ranges.push((start, args_start + raw_args.len()));
                cursor = next_marker;
            }
            Err(msg) => {
                errors.push(msg);
                cursor = next_marker;
            }
        }
    }

    if calls.is_empty() && errors.is_empty() {
        return None;
    }

    let prose = prose_without_ranges(src, &ranges);
    let mut violations = Vec::new();
    if !calls.is_empty() {
        violations.push(
            "Tool call(s) were emitted with Mistral `[TOOL_CALLS]name[ARGS]{...}` markers. \
             Executed this turn so work moves forward; use `<tool_call>name({ ... })</tool_call>` \
             on subsequent turns."
                .to_string(),
        );
    }

    let canonical = canonical_for_recovered_calls(&calls, &prose);

    Some(TextToolParseResult {
        calls,
        errors,
        prose,
        user_response: None,
        violations,
        recovered_from_stray_count: 0,
        done_marker: None,
        canonical,
        dropped: Vec::new(),
    })
}

fn parse_mistral_json_payload(
    src: &str,
    known: &BTreeSet<String>,
    start_index: usize,
) -> Result<(Vec<serde_json::Value>, usize), String> {
    let mut values = serde_json::Deserializer::from_str(src).into_iter::<serde_json::Value>();
    let value = values
        .next()
        .transpose()
        .map_err(|error| format!("Mistral [TOOL_CALLS] JSON payload did not parse: {error}."))?;
    let Some(value) = value else {
        return Err("Mistral [TOOL_CALLS] JSON payload was empty.".to_string());
    };
    let consumed = values.byte_offset();
    let items = match value {
        serde_json::Value::Array(items) => items,
        item @ serde_json::Value::Object(_) => vec![item],
        other => {
            return Err(format!(
                "Mistral [TOOL_CALLS] JSON payload must be an object or array, got {other}."
            ))
        }
    };
    let mut calls = Vec::new();
    for (offset, item) in items.into_iter().enumerate() {
        let Some(object) = item.as_object() else {
            return Err("Mistral [TOOL_CALLS] JSON entries must be objects.".to_string());
        };
        let name = object
            .get("name")
            .or_else(|| object.get("tool_name"))
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if name.is_empty() {
            return Err("Mistral [TOOL_CALLS] JSON entry was missing `name`.".to_string());
        }
        if !known.contains(name) {
            return Err(format!(
                "Unknown tool '{name}' in Mistral [TOOL_CALLS] JSON payload."
            ));
        }
        let raw_args = object
            .get("arguments")
            .or_else(|| object.get("args"))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let parsed_args = match raw_args {
            serde_json::Value::String(text) => serde_json::from_str::<serde_json::Value>(&text)
                .map_err(|error| {
                    format!("Mistral [TOOL_CALLS] arguments for `{name}` did not parse: {error}.")
                })?,
            value => value,
        };
        let arguments = match parsed_args {
            value if value.is_object() => value,
            serde_json::Value::Null => serde_json::json!({}),
            other => {
                return Err(format!(
                    "Mistral [TOOL_CALLS] arguments for `{name}` must be an object or JSON string containing an object, got {other}."
                ))
            }
        };
        calls.push(serde_json::json!({
            "id": format!("tc_mistral_{}", start_index + offset),
            "name": name,
            "arguments": arguments,
        }));
    }
    Ok((calls, consumed))
}

pub(super) fn parse_deepseek_dsml_calls(
    src: &str,
    tools_val: Option<&VmValue>,
) -> Option<TextToolParseResult> {
    const DSML_MARKER: &str = "<｜DSML｜";
    if !src.contains(DSML_MARKER) {
        return None;
    }

    let known: BTreeSet<String> = collect_tool_schemas(tools_val, None)
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    let invoke_re =
        regex::Regex::new(r#"(?s)<｜DSML｜invoke\s+name="([^"]+)"\s*>(.*?)</｜DSML｜invoke>"#)
            .expect("valid DSML invoke regex");
    let block_re = regex::Regex::new(
        r"(?s)<｜DSML｜function_calls>.*?</｜DSML｜function_calls>|<｜DSML｜tool_calls>.*?</｜DSML｜tool_calls>",
    )
    .expect("valid DSML wrapper regex");
    let param_re = regex::Regex::new(
        r#"(?s)<｜DSML｜parameter\s+name="([^"]+)"(?:\s+string="(true|false)")?\s*>(.*?)</｜DSML｜parameter>"#,
    )
    .expect("valid DSML parameter regex");

    let mut calls = Vec::new();
    let mut errors = Vec::new();
    let mut ranges = Vec::<(usize, usize)>::new();
    for block in block_re.find_iter(src) {
        ranges.push((block.start(), block.end()));
    }
    for captures in invoke_re.captures_iter(src) {
        let whole = captures.get(0).expect("whole DSML invoke match");
        let covered = ranges.iter().any(|(range_start, range_end)| {
            whole.start() >= *range_start && whole.end() <= *range_end
        });
        if !covered {
            ranges.push((whole.start(), whole.end()));
        }
        let name = captures.get(1).map(|m| m.as_str()).unwrap_or("");
        if !known.contains(name) {
            errors.push(format!("Unknown tool '{name}' in DeepSeek DSML invoke."));
            continue;
        }
        let body = captures.get(2).map(|m| m.as_str()).unwrap_or("");
        let mut args = serde_json::Map::new();
        for param in param_re.captures_iter(body) {
            let key = param.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
            let as_string = param.get(2).map(|m| m.as_str()) != Some("false");
            let raw = param.get(3).map(|m| m.as_str()).unwrap_or("");
            let value = if as_string {
                serde_json::Value::String(decode_html_entities(raw))
            } else {
                parse_dsml_value(raw).unwrap_or_else(|error| {
                    errors.push(format!(
                        "DeepSeek DSML parameter `{key}` for `{name}` could not parse as JSON: {error}."
                    ));
                    serde_json::Value::String(decode_html_entities(raw))
                })
            };
            args.insert(key, value);
        }
        calls.push(serde_json::json!({
            "id": format!("tc_dsml_{}", calls.len()),
            "name": name,
            "arguments": serde_json::Value::Object(args),
        }));
    }
    let mut marker_cursor = 0usize;
    while let Some(relative) = src[marker_cursor..].find(DSML_MARKER) {
        let start = marker_cursor + relative;
        let covered = ranges
            .iter()
            .any(|(range_start, range_end)| start >= *range_start && start < *range_end);
        if !covered {
            ranges.push((start, src.len()));
            break;
        }
        marker_cursor = start + DSML_MARKER.len();
    }
    ranges.sort_by_key(|(start, _)| *start);

    if calls.is_empty() && errors.is_empty() {
        return None;
    }

    let prose = prose_without_ranges(src, &ranges);
    let canonical = canonical_for_recovered_calls(&calls, &prose);

    Some(TextToolParseResult {
        calls,
        errors,
        prose,
        user_response: None,
        violations: Vec::new(),
        recovered_from_stray_count: 0,
        done_marker: None,
        canonical,
        dropped: Vec::new(),
    })
}

fn parse_dsml_value(raw: &str) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::from_str(raw.trim())
}
