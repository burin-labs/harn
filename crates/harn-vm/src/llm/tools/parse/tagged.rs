use std::collections::BTreeSet;

use super::super::{text_tool_call_block, TEXT_TOOL_CALL_TAG, TEXT_TOOL_CALL_TAG_COMPACT};
use super::bare::parse_bare_calls_in_body;
use super::syntax::{
    collapse_blank_lines, ident_length, match_block, parse_ts_call_from, preview_str,
    render_canonical_call, skip_heredoc_body, strip_thinking_tags,
};
use super::TextToolParseResult;
use crate::llm::tools::collect_tool_schemas;
use crate::value::VmValue;

/// Parse a model response under the strict tagged response protocol.
///
/// The grammar accepts a sequence of top-level blocks separated by
/// whitespace only:
///
/// ```text
///   <tool_call> <bare `name({...})` expression> </tool_call>
///   <assistant_prose> short narration </assistant_prose>
///   <user_response> final user-facing answer </user_response>
///   <done>##DONE##</done>
/// ```
///
/// Anything else at the top level — stray prose, code, unknown tags,
/// unclosed tags — is reported as a `violation`. Malformed call bodies
/// are reported as `errors` (per-call diagnostics). The function always
/// runs to completion so every violation can be surfaced to the model
/// on the next turn.
///
/// The `canonical` field is the response re-emitted in the tagged form.
/// It's what should be replayed as the assistant history entry, not the
/// raw provider bytes — that closes the self-poison loop where a turn
/// with leading raw code becomes "what the agent said" on the next turn.
pub(crate) fn parse_text_tool_calls_with_tools(
    text: &str,
    tools_val: Option<&VmValue>,
) -> TextToolParseResult {
    let cleaned = strip_thinking_tags(text);
    let src = cleaned.as_ref();
    if let Some(result) = parse_mistral_marker_calls(src, tools_val) {
        return result;
    }
    if let Some(result) = parse_deepseek_dsml_calls(src, tools_val) {
        return result;
    }

    let mut calls: Vec<serde_json::Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut violations: Vec<String> = Vec::new();
    let mut assistant_prose_parts: Vec<String> = Vec::new();
    let mut user_response_parts: Vec<String> = Vec::new();
    let mut canonical_parts: Vec<String> = Vec::new();
    let mut done_marker: Option<String> = None;

    let mut cursor = 0usize;
    let bytes = src.as_bytes();

    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            break;
        }

        // Skip past `<<TAG ... TAG` heredoc bodies inline so a bare
        // `name({ key: <<EOF ... EOF })` survives the chunker.
        if bytes[cursor] != b'<' {
            let start = cursor;
            loop {
                while cursor < bytes.len() && bytes[cursor] != b'<' {
                    cursor += 1;
                }
                if cursor + 1 < bytes.len() && bytes[cursor] == b'<' && bytes[cursor + 1] == b'<' {
                    if let Some(after) = skip_heredoc_body(src, cursor) {
                        cursor = after;
                        continue;
                    }
                }
                break;
            }
            report_stray(
                &src[start..cursor],
                &mut violations,
                tools_val,
                &mut calls,
                &mut canonical_parts,
            );
            continue;
        }

        if !is_top_level_tag_position(src, cursor) || inside_markdown_fence(src, cursor) {
            let start = cursor;
            while cursor < bytes.len() && bytes[cursor] != b'\n' {
                cursor += 1;
            }
            report_stray(
                &src[start..cursor],
                &mut violations,
                tools_val,
                &mut calls,
                &mut canonical_parts,
            );
            continue;
        }

        if let Some((body, after)) = match_block(src, cursor, TEXT_TOOL_CALL_TAG)
            .or_else(|| match_block(src, cursor, TEXT_TOOL_CALL_TAG_COMPACT))
        {
            match parse_single_tool_call(body, tools_val) {
                Ok(call) => {
                    let name = call
                        .get("name")
                        .and_then(|name| name.as_str())
                        .unwrap_or("")
                        .to_string();
                    let args = call
                        .get("arguments")
                        .cloned()
                        .unwrap_or(serde_json::json!({}));
                    canonical_parts
                        .push(text_tool_call_block(&render_canonical_call(&name, &args)));
                    calls.push(call);
                }
                Err(msg) => errors.push(msg),
            }
            cursor = after;
        } else if let Some((body, after)) = match_block(src, cursor, "assistant_prose")
            .or_else(|| match_block(src, cursor, "assistantprose"))
        {
            let trimmed = body.trim();
            if !trimmed.is_empty() {
                assistant_prose_parts.push(trimmed.to_string());
                canonical_parts.push(format!("<assistant_prose>\n{trimmed}\n</assistant_prose>"));
            }
            cursor = after;
        } else if let Some((body, after)) = match_block(src, cursor, "user_response")
            .or_else(|| match_block(src, cursor, "userresponse"))
        {
            let trimmed = body.trim();
            if !trimmed.is_empty() {
                user_response_parts.push(trimmed.to_string());
                canonical_parts.push(format!("<user_response>\n{trimmed}\n</user_response>"));
            }
            cursor = after;
        } else if let Some((body, after)) = match_block(src, cursor, "done") {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                violations.push(
                    "<done> block is empty. Emit the configured done sentinel \
                     (default `##DONE##`) inside the block."
                        .to_string(),
                );
            } else {
                done_marker = Some(trimmed.to_string());
                canonical_parts.push(format!("<done>{trimmed}</done>"));
            }
            cursor = after;
        } else if let Some((call, after_call)) =
            try_parse_angle_wrapped_call(src, cursor, tools_val)
        {
            // `<name({...})>` — Qwen fallback when the chat template
            // wraps tools in generic XML brackets. Execute + record a
            // soft violation so the model uses `<tool_call>` next turn.
            let name = call
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or("")
                .to_string();
            let args = call
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            canonical_parts.push(text_tool_call_block(&render_canonical_call(&name, &args)));
            calls.push(call);
            violations.push(format!(
                "Tool call `{name}` was emitted as `<{name}(...)>` instead of \
                 `<tool_call>{name}({{ ... }})</tool_call>`. Executed this turn \
                 so work moves forward; wrap each call in `<tool_call>` tags on \
                 subsequent turns."
            ));
            cursor = after_call;
        } else {
            // Unclosed/unknown tag — skip to end of line or `>`.
            let start = cursor;
            let mut end = cursor + 1;
            while end < bytes.len() && bytes[end] != b'>' && bytes[end] != b'\n' {
                end += 1;
            }
            if end < bytes.len() && bytes[end] == b'>' {
                end += 1;
            }
            let fragment = &src[start..end];
            if fragment.starts_with('<') && !fragment.contains('>') {
                violations.push(format!(
                    "Unclosed tag starting at {:?}. Close it or remove it; only \
                     <tool_call>, <assistant_prose>, <user_response>, and <done> are accepted.",
                    preview_str(fragment, 40)
                ));
            } else {
                violations.push(format!(
                    "Unknown top-level tag {:?}. Use <tool_call>, <assistant_prose>, \
                     <user_response>, or <done> — no other tags are accepted at the top level.",
                    preview_str(fragment, 40)
                ));
            }
            cursor = end;
        }
    }

    let response_is_effectively_empty = calls.is_empty()
        && assistant_prose_parts.is_empty()
        && user_response_parts.is_empty()
        && done_marker.is_none()
        && violations.is_empty()
        && errors.is_empty();
    if response_is_effectively_empty && !src.trim().is_empty() {
        violations.push(
            "Response contained no <tool_call>, <assistant_prose>, <user_response>, or <done> block. \
             Every response must be composed of these tags only."
                .to_string(),
        );
    }
    let user_response = if user_response_parts.is_empty() {
        None
    } else {
        Some(user_response_parts.join("\n\n"))
    };

    TextToolParseResult {
        calls,
        errors,
        prose: user_response
            .clone()
            .unwrap_or_else(|| assistant_prose_parts.join("\n\n")),
        user_response,
        violations,
        done_marker,
        canonical: canonical_parts.join("\n\n"),
    }
}

fn parse_mistral_marker_calls(
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
        done_marker: None,
        canonical,
    })
}

fn parse_deepseek_dsml_calls(
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
    let block_re = regex::Regex::new(r#"(?s)<｜DSML｜function_calls>.*?</｜DSML｜function_calls>"#)
        .expect("valid DSML function_calls regex");
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
                serde_json::Value::String(decode_dsml_text(raw))
            } else {
                parse_dsml_value(raw).unwrap_or_else(|error| {
                    errors.push(format!(
                        "DeepSeek DSML parameter `{key}` for `{name}` could not parse as JSON: {error}."
                    ));
                    serde_json::Value::String(decode_dsml_text(raw))
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
    let mut violations = Vec::new();
    if !calls.is_empty() {
        violations.push(
            "Tool call(s) were emitted with DeepSeek DSML markers. Executed this turn so work moves \
             forward; use `<tool_call>name({ ... })</tool_call>` on subsequent turns."
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
        done_marker: None,
        canonical,
    })
}

fn parse_dsml_value(raw: &str) -> Result<serde_json::Value, serde_json::Error> {
    serde_json::from_str(raw.trim())
}

fn decode_dsml_text(raw: &str) -> String {
    raw.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

fn canonical_for_recovered_calls(calls: &[serde_json::Value], prose: &str) -> String {
    let mut canonical_parts = Vec::new();
    if !prose.trim().is_empty() {
        canonical_parts.push(format!(
            "<assistant_prose>\n{}\n</assistant_prose>",
            prose.trim()
        ));
    }
    for call in calls {
        let name = call
            .get("name")
            .and_then(|name| name.as_str())
            .unwrap_or("");
        let args = call
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));
        canonical_parts.push(text_tool_call_block(&render_canonical_call(name, &args)));
    }
    canonical_parts.join("\n\n")
}

fn prose_without_ranges(src: &str, ranges: &[(usize, usize)]) -> String {
    if ranges.is_empty() {
        return collapse_blank_lines(src).trim().to_string();
    }
    let mut out = String::new();
    let mut cursor = 0usize;
    for (start, end) in ranges {
        if *start > cursor {
            out.push_str(&src[cursor..*start]);
        }
        cursor = (*end).max(cursor);
    }
    if cursor < src.len() {
        out.push_str(&src[cursor..]);
    }
    collapse_blank_lines(&out).trim().to_string()
}

/// Try to parse `<name({...})>` (or `<name({...})` with the closing `>`
/// optional / on a later line) at `cursor`. Returns the parsed call and
/// the byte position after the call (including any trailing `>`).
/// Only succeeds when `name` resolves to a registered tool.
fn try_parse_angle_wrapped_call(
    src: &str,
    cursor: usize,
    tools_val: Option<&VmValue>,
) -> Option<(serde_json::Value, usize)> {
    let bytes = src.as_bytes();
    if bytes.get(cursor) != Some(&b'<') {
        return None;
    }
    // Identifier immediately after `<`.
    let name_start = cursor + 1;
    let name_len = ident_length(&bytes[name_start..])?;
    if name_len == 0 {
        return None;
    }
    if bytes.get(name_start + name_len) != Some(&b'(') {
        return None;
    }
    let name_str = std::str::from_utf8(&bytes[name_start..name_start + name_len]).ok()?;
    // Only known tools are eligible — keeps `<notes>...` out of the path.
    let known: BTreeSet<String> = collect_tool_schemas(tools_val, None)
        .into_iter()
        .map(|schema| schema.name)
        .chain(["ledger".to_string(), "load_skill".to_string()])
        .collect();
    if !known.contains(name_str) {
        return None;
    }
    // Reuse the TS-call parser. It scans for the matching `)` honoring
    // heredocs, template literals, and nested object/array literals, so
    // multi-line calls with `<<EOF ... EOF` bodies are handled.
    let (arguments, consumed) =
        parse_ts_call_from(&src[name_start..], name_str.to_string()).ok()?;
    let mut end = name_start + consumed;
    // Step past optional whitespace and a single trailing `>`.
    while end < bytes.len() && (bytes[end] == b' ' || bytes[end] == b'\t') {
        end += 1;
    }
    if bytes.get(end) == Some(&b'>') {
        end += 1;
    }
    let call = serde_json::json!({
        "id": format!("tc_angle_{name_str}"),
        "name": name_str,
        "arguments": arguments,
    });
    Some((call, end))
}

fn is_top_level_tag_position(src: &str, cursor: usize) -> bool {
    let line_start = src[..cursor].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    src[line_start..cursor]
        .chars()
        .all(|ch| matches!(ch, ' ' | '\t' | '\r'))
}

fn inside_markdown_fence(src: &str, cursor: usize) -> bool {
    let mut count = 0;
    let mut scan = 0;
    while scan < cursor {
        let Some(pos) = src[scan..cursor].find("```") else {
            break;
        };
        count += 1;
        scan += pos + 3;
    }
    count % 2 == 1
}

/// Report stray text that sits outside any recognized top-level tag.
/// When the stray content contains parseable tool calls, execute them
/// (route them through the canonical-call path) and add a soft violation
/// so the model still gets the signal to wrap calls properly. Pre-v0.5.82
/// the parser flagged-and-dropped these calls, which was correct in
/// principle but stranded weaker locally-hosted models in loops where
/// they kept re-emitting the same right-shape-wrong-wrapper response.
fn report_stray(
    fragment: &str,
    violations: &mut Vec<String>,
    tools_val: Option<&VmValue>,
    calls: &mut Vec<serde_json::Value>,
    canonical_parts: &mut Vec<String>,
) {
    let trimmed = fragment.trim();
    if trimmed.is_empty() {
        return;
    }
    let sniff = parse_bare_calls_in_body(trimmed, tools_val);
    if !sniff.calls.is_empty() {
        let names: Vec<_> = sniff
            .calls
            .iter()
            .filter_map(|call| {
                call.get("name")
                    .and_then(|name| name.as_str())
                    .map(|name| name.to_string())
            })
            .collect();
        for call in &sniff.calls {
            let name = call
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or("")
                .to_string();
            let args = call
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            canonical_parts.push(text_tool_call_block(&render_canonical_call(&name, &args)));
            calls.push(call.clone());
        }
        violations.push(format!(
            "Tool call(s) ({}) were emitted as bare text outside `<tool_call>` tags. \
             Executed this turn so work moves forward; please wrap each call in \
             `<tool_call>...</tool_call>` on subsequent turns.",
            names.join(", ")
        ));
    } else {
        violations.push(format!(
            "Stray text outside response tags: {:?}. Wrap all prose in \
             <assistant_prose>...</assistant_prose> or <user_response>...</user_response>, \
             and every tool call in <tool_call>...</tool_call>.",
            preview_str(trimmed, 120)
        ));
    }
}

/// Parse a single `<tool_call>` body. Expects exactly one bare
/// `name({ ... })` expression (possibly with surrounding whitespace).
fn parse_single_tool_call(
    body: &str,
    tools_val: Option<&VmValue>,
) -> Result<serde_json::Value, String> {
    if let Some(call) = parse_json_tool_call_body(body, tools_val)? {
        return Ok(call);
    }
    let inner = parse_bare_calls_in_body(body, tools_val);
    if let Some(err) = inner.errors.into_iter().next() {
        return Err(err);
    }
    if inner.calls.is_empty() {
        return Err(format!(
            "<tool_call> body did not contain a bare `name({{ ... }})` expression. \
             Got: {:?}",
            preview_str(body.trim(), 120)
        ));
    }
    if inner.calls.len() > 1 {
        return Err(format!(
            "<tool_call> body contained {} calls; emit one call per <tool_call> block.",
            inner.calls.len()
        ));
    }
    Ok(inner.calls.into_iter().next().expect("len == 1"))
}

fn parse_json_tool_call_body(
    body: &str,
    tools_val: Option<&VmValue>,
) -> Result<Option<serde_json::Value>, String> {
    let trimmed = body.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return Ok(None);
    }
    let parsed: serde_json::Value = serde_json::from_str(trimmed).map_err(|error| {
        format!(
            "<tool_call> body looked like JSON but did not parse: {error}. \
             Emit either `name({{ ... }})` or JSON with `name` and `arguments`."
        )
    })?;
    let item = match parsed {
        serde_json::Value::Array(items) if items.len() == 1 => items.into_iter().next().unwrap(),
        serde_json::Value::Array(items) => {
            return Err(format!(
                "<tool_call> JSON array contained {} calls; emit one call per <tool_call> block.",
                items.len()
            ));
        }
        value @ serde_json::Value::Object(_) => value,
        other => {
            return Err(format!(
                "<tool_call> JSON body must be an object, got `{other}`."
            ));
        }
    };
    let obj = item.as_object().expect("JSON object matched above");
    let name = obj
        .get("name")
        .or_else(|| obj.get("tool_name"))
        .or_else(|| {
            obj.get("function")
                .and_then(|function| function.get("name"))
        })
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if name.is_empty() {
        return Err("<tool_call> JSON body did not contain a tool name".to_string());
    }
    let known: BTreeSet<String> = collect_tool_schemas(tools_val, None)
        .into_iter()
        .map(|schema| schema.name)
        .chain(["ledger".to_string(), "load_skill".to_string()])
        .collect();
    if !known.contains(name) {
        let available: Vec<_> = known.iter().take(20).cloned().collect();
        return Err(format!(
            "Unknown tool '{}'. Available tools: [{}]",
            name,
            available.join(", ")
        ));
    }
    let arguments = obj
        .get("arguments")
        .or_else(|| obj.get("parameters"))
        .or_else(|| obj.get("args"))
        .or_else(|| {
            obj.get("function")
                .and_then(|function| function.get("arguments"))
        })
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let arguments = match arguments {
        serde_json::Value::String(raw) => serde_json::from_str(&raw).map_err(|error| {
            format!("Could not parse JSON string arguments for tool '{name}': {error}")
        })?,
        value => value,
    };
    if !arguments.is_object() {
        return Err(format!(
            "Tool '{name}' arguments must be a JSON object, got `{arguments}`."
        ));
    }
    let id = obj
        .get("id")
        .and_then(|id| id.as_str())
        .unwrap_or("tc_json");
    Ok(Some(serde_json::json!({
        "id": id,
        "name": name,
        "arguments": arguments,
    })))
}
