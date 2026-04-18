//! Text tool-call parsing: the reverse-direction wire format used by the
//! agent loop to read tool invocations back out of a model response.
//!
//! Exposes `parse_text_tool_calls_with_tools` + `parse_bare_calls_in_body`
//! and the `TextToolParseResult` shape; everything else is a local helper
//! (ident parser, TS literal parser, heredoc skipper, native-JSON fallback).

use std::collections::BTreeSet;

use super::collect_tool_schemas;
use super::ts_value_parser::TsValueParser;
use crate::value::VmValue;

/// Result of parsing a prose-interleaved TS tool-call stream.
///
/// The scanner walks the model's text once and splits it into three
/// streams for the caller:
///   - `calls`: the parsed structured tool calls.
///   - `errors`: diagnostics for malformed call attempts.
///   - `prose`: the original text with every successfully-parsed call
///     expression removed, whitespace around the hole collapsed. This is
///     what should be shown as "the agent's answer" and replayed back into
///     conversation history — tool calls are structured data, not narration.
pub(crate) struct TextToolParseResult {
    pub calls: Vec<serde_json::Value>,
    pub errors: Vec<String>,
    pub prose: String,
    /// Protocol-level grammar violations (stray text outside tags, unknown
    /// tags, unclosed tags, malformed `<done>` contents). Distinct from
    /// `errors`, which carry per-call parse diagnostics. The agent loop
    /// replays these to the model as structured `protocol_violation`
    /// feedback so it can self-correct.
    pub violations: Vec<String>,
    /// Body of the `<done>` block when one was emitted, trimmed of
    /// surrounding whitespace. The agent compares this against the
    /// pipeline's configured `done_sentinel` (default `##DONE##`) to
    /// decide whether to honor completion. Replaces substring matching
    /// against a bare sentinel string.
    pub done_marker: Option<String>,
    /// Canonical reconstruction of the response in the tagged grammar.
    /// Used as the assistant's history entry so future turns see the
    /// well-formed shape instead of the raw provider bytes.
    pub canonical: String,
}

/// Parse every fenceless TS tool call found in a model's text response.
///
/// The model writes prose and tool calls intermixed. A tool call is a
/// TypeScript function expression `name({...})` whose `name` matches a
/// registered tool AND whose call-site `(` immediately follows the name at
/// the start of a line (leading whitespace allowed). Tool names inside
/// Markdown fenced code blocks (```` ``` ````) or inline code spans (`` ` ``)
/// are treated as narration and skipped.
///
/// The returned `prose` field is the input text with every successfully
/// parsed call expression excised — useful for building a clean "what the
/// model said" string separate from the structured tool-call list.
/// Strip leaked thinking tags from model output. Some models (Qwen, Gemma)
/// emit `</think>` or `<think>` markers in their response text when the
/// streaming transport merges thinking and content channels. These tags
/// break tool-call parsing because they appear between or before valid
/// tool invocations.
fn strip_thinking_tags(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains("<think>") && !text.contains("</think>") {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut result = text.to_string();
    while let Some(start) = result.find("<think>") {
        if let Some(end) = result[start..].find("</think>") {
            result.replace_range(start..start + end + "</think>".len(), "");
        } else {
            result.replace_range(start..start + "<think>".len(), "");
        }
    }
    while result.contains("</think>") {
        result = result.replace("</think>", "");
    }
    std::borrow::Cow::Owned(result)
}

/// Scan a text body for bare `name({ ... })` tool calls and diagnostics.
///
/// This is the body-level parser used inside `<tool_call>` tags by the
/// tagged-protocol scanner. It is also called on whole responses as a
/// diagnostic fallback: when a model emits calls without wrapping them in
/// `<tool_call>` tags, we detect the calls here, report a grammar violation
/// at the outer layer, and refuse to execute until the model re-emits
/// properly wrapped.
pub(crate) fn parse_bare_calls_in_body(
    text: &str,
    tools_val: Option<&VmValue>,
) -> TextToolParseResult {
    let cleaned = strip_thinking_tags(text);
    let text = cleaned.as_ref();

    if let Some(unwrapped) = unwrap_exact_code_wrapper(text) {
        let result = parse_bare_calls_in_body(unwrapped, tools_val);
        if !result.calls.is_empty() || !result.errors.is_empty() {
            return result;
        }
    }
    let mut known: BTreeSet<String> = collect_tool_schemas(tools_val, None)
        .into_iter()
        .map(|s| s.name)
        .collect();
    // Runtime-owned pseudo-tools (handled in the agent runtime, not in
    // the user-declared tool registry).
    known.insert("ledger".to_string());
    known.insert("load_skill".to_string());
    let mut calls = Vec::new();
    let mut errors = Vec::new();
    // Byte ranges excised from the original text to form `prose`.
    let mut call_ranges: Vec<(usize, usize)> = Vec::new();

    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut at_line_start = true;
    let mut in_inline_code = false;
    // (fence_start, fence_end, calls_before_count) — fences bracketing
    // tool calls are added to call_ranges after the scan.
    let mut fence_lines: Vec<(usize, usize, usize)> = Vec::new();

    while i < bytes.len() {
        if at_line_start && !in_inline_code {
            let mut j = i;
            while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                j += 1;
            }
            // Skip fence lines themselves but NOT their content: models
            // routinely wrap tool calls in ```python fences, and skipping
            // the content silently drops ~24% of real calls.
            if bytes.get(j) == Some(&b'`')
                && bytes.get(j + 1) == Some(&b'`')
                && bytes.get(j + 2) == Some(&b'`')
            {
                let fence_start = i;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
                fence_lines.push((fence_start, i, calls.len()));
                at_line_start = true;
                continue;
            }
            {
                // Strip model-generated prefixes before the tool name:
                // `call:`, `tool:` (Qwen also uses `<read(...)>`), and
                // Gemma's native `tool_code:`. `python:`/`javascript:`
                // are language-tag labels some models add when they
                // think the runtime wants a code block.
                let mut k = j;
                if bytes.get(k) == Some(&b'<') {
                    k += 1;
                    while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
                        k += 1;
                    }
                }
                for prefix in [
                    "tool_code:",
                    "tool_call:",
                    "tool_output:",
                    "call:",
                    "tool:",
                    "use:",
                    "python:",
                    "javascript:",
                    "typescript:",
                    "shell:",
                    "bash:",
                ] {
                    if text[k..].starts_with(prefix) {
                        k += prefix.len();
                        // Also skip optional whitespace after the prefix.
                        while k < bytes.len() && (bytes[k] == b' ' || bytes[k] == b'\t') {
                            k += 1;
                        }
                        break;
                    }
                }
                // Near-miss: `some_label: known_tool(...)` where the
                // label isn't in our strip allowlist. Emit a diagnostic
                // (no execution) so the model self-corrects. Guarded by
                // the SECOND identifier being a known tool to avoid
                // false positives on prose like `Tip: edit(...)`.
                if let Some(label_len) = ident_length(&bytes[k..]) {
                    if bytes.get(k + label_len) == Some(&b':') {
                        let mut after_colon = k + label_len + 1;
                        while after_colon < bytes.len()
                            && (bytes[after_colon] == b' ' || bytes[after_colon] == b'\t')
                        {
                            after_colon += 1;
                        }
                        if let Some(inner_len) = ident_length(&bytes[after_colon..]) {
                            if bytes.get(after_colon + inner_len) == Some(&b'(') {
                                let inner_name = std::str::from_utf8(
                                    &bytes[after_colon..after_colon + inner_len],
                                )
                                .unwrap_or("");
                                if known.contains(inner_name) {
                                    let label =
                                        std::str::from_utf8(&bytes[k..k + label_len]).unwrap_or("");
                                    errors.push(format!(
                                        "Saw `{label}: {inner_name}(...)`. Do not prefix tool \
                                         calls with `{label}:` — emit bare \
                                         `{inner_name}({{ ... }})` on its own line. The \
                                         previous line was treated as prose and no tool \
                                         ran; re-emit it without the prefix."
                                    ));
                                    while i < bytes.len() && bytes[i] != b'\n' {
                                        i += 1;
                                    }
                                    continue;
                                }
                            }
                        }
                    }
                }

                if let Some(name_len) = ident_length(&bytes[k..]) {
                    if bytes.get(k + name_len) == Some(&b'(') {
                        let name_str = std::str::from_utf8(&bytes[k..k + name_len]).unwrap_or("");
                        let object_arg_start = has_object_literal_arg_start(text, k + name_len + 1);
                        if known.contains(name_str) {
                            if !object_arg_start {
                                errors.push(format!(
                                    "Tool '{}' must be called with an object literal argument like {}({{ ... }}).",
                                    name_str, name_str
                                ));
                                i = k + name_len + 1;
                                at_line_start = false;
                                continue;
                            }
                            let name = name_str.to_string();
                            match parse_ts_call_from(&text[k..], name.clone()) {
                                Ok((arguments, consumed)) => {
                                    calls.push(serde_json::json!({
                                        "id": format!("tc_{}", calls.len()),
                                        "name": name,
                                        "arguments": arguments,
                                    }));
                                    // Use j (original line start) so the
                                    // prefix is excised too. Consume trailing
                                    // `>` when the call was angle-wrapped.
                                    let mut end = k + consumed;
                                    while end < bytes.len()
                                        && (bytes[end] == b' ' || bytes[end] == b'\t')
                                    {
                                        end += 1;
                                    }
                                    if end < bytes.len() && bytes[end] == b'>' {
                                        end += 1;
                                    }
                                    call_ranges.push((j, end));
                                    i = end;
                                    at_line_start = bytes.get(i.saturating_sub(1)) == Some(&b'\n');
                                    continue;
                                }
                                Err(msg) => {
                                    errors.push(msg);
                                    i = k + name_len + 1;
                                    at_line_start = false;
                                    continue;
                                }
                            }
                        } else if object_arg_start {
                            let available: Vec<_> = known.iter().take(20).cloned().collect();
                            errors.push(format!(
                                "Unknown tool '{}'. Available tools: [{}]",
                                name_str,
                                available.join(", ")
                            ));
                            i = k + name_len + 1;
                            at_line_start = false;
                            continue;
                        }
                    }
                }
            }
        }

        // Tool names inside inline code spans are references, not calls.
        if bytes[i] == b'`' {
            in_inline_code = !in_inline_code;
            at_line_start = false;
            i += 1;
            continue;
        }

        if bytes[i] == b'\n' {
            at_line_start = true;
        } else if !bytes[i].is_ascii_whitespace() {
            at_line_start = false;
        }
        i += 1;
    }

    // Strip fence lines bracketing tool calls (formatting wrappers).
    for pair in fence_lines.windows(2) {
        let (open_start, open_end, calls_before_open) = pair[0];
        let (_close_start, close_end, calls_before_close) = pair[1];
        if calls_before_close > calls_before_open {
            call_ranges.push((open_start, open_end));
            call_ranges.push((_close_start, close_end));
        }
    }
    // Handle a trailing unclosed fence.
    if fence_lines.len() % 2 == 1 {
        let (start, end, calls_before) = *fence_lines.last().unwrap();
        if calls.len() > calls_before {
            call_ranges.push((start, end));
        }
    }
    call_ranges.sort_by_key(|r| r.0);

    // Strip empty fence pairs: models emit them as failed tool-call
    // attempts and they cause duplication loops in conversation history.
    for pair in fence_lines.windows(2) {
        let (open_start, _open_end, calls_before_open) = pair[0];
        let (_close_start, close_end, calls_before_close) = pair[1];
        if calls_before_close == calls_before_open {
            call_ranges.push((open_start, close_end));
        }
    }
    call_ranges.sort_by_key(|r| r.0);
    call_ranges.dedup_by(|b, a| a.0 == b.0);

    let prose = if call_ranges.is_empty() {
        strip_empty_fences(text)
    } else {
        let mut buf = String::with_capacity(text.len());
        let mut cursor = 0usize;
        for (start, end) in &call_ranges {
            if *start > cursor {
                buf.push_str(&text[cursor..*start]);
            }
            cursor = *end;
        }
        if cursor < text.len() {
            buf.push_str(&text[cursor..]);
        }
        collapse_blank_lines(&strip_empty_fences(&buf))
            .trim()
            .to_string()
    };

    // Fallback: some function-calling-trained models ignore text-format
    // instructions and emit `[{"id":"call_...","function":{...}}]` raw.
    // Parse and execute directly instead of wasting an iteration.
    if calls.is_empty() && errors.is_empty() {
        let (native_calls, native_errors) = parse_native_json_tool_calls(text, &known);
        if !native_calls.is_empty() || !native_errors.is_empty() {
            return TextToolParseResult {
                calls: native_calls,
                errors: native_errors,
                prose: String::new(),
                violations: Vec::new(),
                done_marker: None,
                canonical: String::new(),
            };
        }
    }

    TextToolParseResult {
        calls,
        errors,
        prose,
        violations: Vec::new(),
        done_marker: None,
        canonical: String::new(),
    }
}

/// Parse a model response under the strict tagged response protocol.
///
/// The grammar accepts a sequence of top-level blocks separated by
/// whitespace only:
///
/// ```text
///   <tool_call> <bare `name({...})` expression> </tool_call>
///   <assistant_prose> short narration </assistant_prose>
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
    let mut prose_parts: Vec<String> = Vec::new();
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
                        .and_then(|n| n.as_str())
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
                prose_parts.push(trimmed.to_string());
                canonical_parts.push(format!("<assistant_prose>\n{trimmed}\n</assistant_prose>"));
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
                .and_then(|n| n.as_str())
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
                     <tool_call>, <assistant_prose>, and <done> are accepted.",
                    preview_str(fragment, 40)
                ));
            } else {
                violations.push(format!(
                    "Unknown top-level tag {:?}. Use <tool_call>, <assistant_prose>, \
                     or <done> — no other tags are accepted at the top level.",
                    preview_str(fragment, 40)
                ));
            }
            cursor = end;
        }
    }

    let response_is_effectively_empty = calls.is_empty()
        && prose_parts.is_empty()
        && done_marker.is_none()
        && violations.is_empty()
        && errors.is_empty();
    if response_is_effectively_empty && !src.trim().is_empty() {
        violations.push(
            "Response contained no <tool_call>, <assistant_prose>, or <done> block. \
             Every response must be composed of these tags only."
                .to_string(),
        );
    }

    TextToolParseResult {
        calls,
        errors,
        prose: prose_parts.join("\n\n"),
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
        .map(|s| s.name)
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
            .filter_map(|c| {
                c.get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string())
            })
            .collect();
        for call in &sniff.calls {
            let name = call
                .get("name")
                .and_then(|n| n.as_str())
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
             <assistant_prose>...</assistant_prose> and every tool call in \
             <tool_call>...</tool_call>.",
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

/// Match a balanced `<tag>...</tag>` block starting at `start` in `src`.
/// Returns `(body_slice, end_cursor)` on success. Does not support nested
/// same-name tags — not needed for this grammar and attempting to support
/// them bloats the error surface for no real benefit.
fn match_block<'a>(src: &'a str, start: usize, tag: &str) -> Option<(&'a str, usize)> {
    let open = format!("<{tag}>");
    if !src[start..].starts_with(&open) {
        return None;
    }
    let body_start = start + open.len();
    let close = format!("</{tag}>");
    let close_idx = src[body_start..].find(&close)?;
    let body_end = body_start + close_idx;
    let after = body_end + close.len();
    Some((&src[body_start..body_end], after))
}

/// Render a parsed tool call back to the bare TS syntax used inside
/// `<tool_call>` tags. Used to build the canonical history entry.
fn render_canonical_call(name: &str, args: &serde_json::Value) -> String {
    // JSON object literals are accepted by our tool-call grammar, so
    // pretty-printed JSON is sufficient for replay.
    let rendered_args = serde_json::to_string_pretty(args).unwrap_or_else(|_| "{}".to_string());
    format!("{name}({rendered_args})")
}

fn preview_str(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let kept: String = chars.into_iter().take(max).collect();
    format!("{kept}…")
}

fn has_object_literal_arg_start(text: &str, open_paren_idx: usize) -> bool {
    let bytes = text.as_bytes();
    let mut idx = open_paren_idx;
    while idx < bytes.len() && (bytes[idx] == b' ' || bytes[idx] == b'\t') {
        idx += 1;
    }
    bytes.get(idx) == Some(&b'{')
}

/// Detect and parse OpenAI-style native function calling JSON that a model
/// emitted as raw text. Looks for `[{"id":"call_...","function":{"name":"...",
/// "arguments":"..."}}]` patterns (array or single object) embedded anywhere
/// in the text.
pub(crate) fn parse_native_json_tool_calls(
    text: &str,
    known_tools: &BTreeSet<String>,
) -> (Vec<serde_json::Value>, Vec<String>) {
    let mut results = Vec::new();
    let mut errors = Vec::new();

    let json_start = text
        .find("[{\"id\":")
        .or_else(|| text.find("[{\"id\":"))
        .or_else(|| text.find("{\"id\":\"call_"));

    let Some(start) = json_start else {
        return (results, errors);
    };

    let json_text = &text[start..];
    let parsed: Option<Vec<serde_json::Value>> = serde_json::from_str(json_text)
        .ok()
        .or_else(|| {
            serde_json::from_str::<serde_json::Value>(json_text)
                .ok()
                .map(|v| vec![v])
        })
        .or_else(|| {
            // Salvage trailing-text JSON by scanning for a valid close.
            for end in (start + 10..text.len()).rev() {
                let slice = &text[start..=end];
                if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(slice) {
                    return Some(arr);
                }
            }
            None
        });

    let Some(items) = parsed else {
        return (results, errors);
    };

    for item in items {
        let func = item.get("function").and_then(|f| f.as_object());
        let Some(func) = func else { continue };
        let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if name.is_empty() {
            continue;
        }
        if !known_tools.contains(name) {
            let available: Vec<_> = known_tools.iter().take(20).cloned().collect();
            errors.push(format!(
                "Unknown tool '{}'. Available tools: [{}]",
                name,
                available.join(", ")
            ));
            continue;
        }
        // OpenAI format encodes arguments as a JSON string; others as an object.
        let arguments = match func.get("arguments") {
            Some(serde_json::Value::String(s)) => match serde_json::from_str(s) {
                Ok(v) => v,
                Err(e) => {
                    errors.push(format!(
                        "Could not parse arguments for tool '{}': {}. Raw: {}",
                        name,
                        e,
                        &s[..s.len().min(200)]
                    ));
                    continue;
                }
            },
            Some(obj @ serde_json::Value::Object(_)) => obj.clone(),
            _ => serde_json::Value::Object(Default::default()),
        };
        let call_id = item
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("native_fallback");
        results.push(serde_json::json!({
            "id": call_id,
            "name": name,
            "arguments": arguments,
        }));
    }

    (results, errors)
}

fn unwrap_exact_code_wrapper(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        let newline = rest.find('\n')?;
        let after_opener = &rest[newline + 1..];
        let inner = after_opener.strip_suffix("```")?;
        return Some(inner.trim());
    }
    let inner = trimmed.strip_prefix('`')?.strip_suffix('`')?;
    if inner.contains('`') {
        return None;
    }
    Some(inner.trim())
}

/// Collapse runs of ≥3 consecutive newlines down to 2 (one blank line). Used
/// to tidy the `prose` output after tool-call ranges are excised, so the
/// removed bytes don't leave an ugly vertical gap between surrounding prose.
/// Strip empty Markdown fence pairs (```lang\n``` or ```lang\n\n```) from text.
/// Models sometimes emit these as failed tool-call attempts. If left in prose
/// they accumulate in conversation history and cause duplication loops.
fn strip_empty_fences(text: &str) -> String {
    let re = regex::Regex::new(r"(?m)^[ \t]*```[^\n]*\n\s*```[ \t]*\n?").unwrap();
    re.replace_all(text, "").to_string()
}

fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut newline_run = 0usize;
    for ch in text.chars() {
        if ch == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                out.push(ch);
            }
        } else {
            newline_run = 0;
            out.push(ch);
        }
    }
    out
}

/// Skip past a `<<TAG\n...\nTAG` heredoc body starting at `start` in `src`.
/// Returns the byte position immediately after the closing tag (mirroring
/// `Parser::parse_heredoc`'s rewind), or `None` when the heredoc is malformed
/// or unterminated. Used by the top-level scanner so a stray-bytes chunker
/// doesn't truncate bare `name({ key: <<EOF\n...\nEOF })` tool calls at the
/// `<<` opener.
fn skip_heredoc_body(src: &str, start: usize) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.get(start) != Some(&b'<') || bytes.get(start + 1) != Some(&b'<') {
        return None;
    }
    let mut pos = start + 2;
    let has_quote = matches!(bytes.get(pos), Some(b'\'') | Some(b'"'));
    let quote_char = bytes.get(pos).copied();
    if has_quote {
        pos += 1;
    }
    let tag_start = pos;
    while let Some(b) = bytes.get(pos) {
        if b.is_ascii_alphanumeric() || *b == b'_' {
            pos += 1;
        } else {
            break;
        }
    }
    if pos == tag_start {
        return None;
    }
    let tag = &src[tag_start..pos];
    if has_quote && bytes.get(pos).copied() == quote_char {
        pos += 1;
    }
    if bytes.get(pos) == Some(&b'\r') {
        pos += 1;
    }
    if bytes.get(pos) != Some(&b'\n') {
        return None;
    }
    pos += 1;
    while pos < bytes.len() {
        let line_start = pos;
        while let Some(b) = bytes.get(pos) {
            if *b == b'\n' {
                break;
            }
            pos += 1;
        }
        let line = &src[line_start..pos];
        let leading_ws_len = line.len() - line.trim_start().len();
        let after_ws = &line[leading_ws_len..];
        if let Some(rest) = after_ws.strip_prefix(tag) {
            let at_word_boundary = rest
                .chars()
                .next()
                .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
            if at_word_boundary {
                return Some(line_start + leading_ws_len + tag.len());
            }
        }
        if bytes.get(pos) == Some(&b'\n') {
            pos += 1;
        } else {
            return None;
        }
    }
    None
}

/// Length of a JavaScript-ish identifier starting at bytes[0]. Returns None
/// if the first byte is not a valid identifier start.
pub(super) fn ident_length(bytes: &[u8]) -> Option<usize> {
    if bytes.is_empty() {
        return None;
    }
    let first = bytes[0];
    if !(first.is_ascii_alphabetic() || first == b'_' || first == b'$') {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'$' {
            i += 1;
        } else {
            break;
        }
    }
    Some(i)
}

/// Parse a full `name(args)` TS call expression starting at the beginning of
/// `text`. Returns the parsed argument JSON and the number of bytes consumed
/// (from the start of the name through the closing paren), or an error with
/// a diagnostic suitable to show the model.
fn parse_ts_call_from(text: &str, name: String) -> Result<(serde_json::Value, usize), String> {
    let bytes = text.as_bytes();
    let paren_open = name.len();
    if bytes.get(paren_open) != Some(&b'(') {
        return Err(format!(
            "TOOL CALL PARSE ERROR: `{name}(` expected immediately after the tool name."
        ));
    }
    let mut p = TsValueParser::new(&text[paren_open + 1..]);
    p.skip_ws_and_comments();
    // An empty arg list `name()` is legal and produces an empty object.
    let args_value = if p.peek() == Some(b')') {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        p.parse_value().map_err(|e| {
            format!(
                "TOOL CALL PARSE ERROR: `{name}(...)` — {e}. \
                 Tool arguments must be a TypeScript object literal: `{{ key: value, key: value }}`."
            )
        })?
    };
    p.skip_ws_and_comments();
    if p.peek() != Some(b')') {
        return Err(format!(
            "TOOL CALL PARSE ERROR: `{name}(...)` — missing closing `)`. \
             Every tool call must be a complete TypeScript expression."
        ));
    }
    let consumed_in_parser = p.position();
    let total_consumed = paren_open + 1 + consumed_in_parser + 1; // +1 for the ')'

    // Tool contract: every call takes a single object literal. Bare
    // positional scalars error precisely rather than being promoted.
    match args_value {
        serde_json::Value::Object(map) => Ok((serde_json::Value::Object(map), total_consumed)),
        other => Err(format!(
            "TOOL CALL PARSE ERROR: `{name}(...)` — expected an object literal argument, \
             got `{}`. Wrap the value in braces: `{name}({{ key: value }})`.",
            other
        )),
    }
}
