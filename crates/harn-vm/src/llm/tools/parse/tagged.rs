use std::collections::BTreeSet;

use super::bare::parse_bare_calls_in_body;
use super::syntax::{
    ident_length, match_block, parse_ts_call_from, preview_str, render_canonical_call,
    skip_heredoc_body, strip_thinking_tags,
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

        if let Some((body, after)) = match_block(src, cursor, "tool_call") {
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
                    canonical_parts.push(format!(
                        "<tool_call>\n{}\n</tool_call>",
                        render_canonical_call(&name, &args)
                    ));
                    calls.push(call);
                }
                Err(msg) => errors.push(msg),
            }
            cursor = after;
        } else if let Some((body, after)) = match_block(src, cursor, "assistant_prose") {
            let trimmed = body.trim();
            if !trimmed.is_empty() {
                assistant_prose_parts.push(trimmed.to_string());
                canonical_parts.push(format!("<assistant_prose>\n{trimmed}\n</assistant_prose>"));
            }
            cursor = after;
        } else if let Some((body, after)) = match_block(src, cursor, "user_response") {
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
            canonical_parts.push(format!(
                "<tool_call>\n{}\n</tool_call>",
                render_canonical_call(&name, &args)
            ));
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
            canonical_parts.push(format!(
                "<tool_call>\n{}\n</tool_call>",
                render_canonical_call(&name, &args)
            ));
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
