use std::collections::BTreeSet;

use super::native_json::parse_native_json_tool_calls;
use super::syntax::{
    collapse_blank_lines, has_object_literal_arg_start, ident_length, parse_ts_call_from,
    strip_empty_fences, strip_thinking_tags, unwrap_exact_code_wrapper,
};
use super::TextToolParseResult;
use crate::llm::tools::collect_tool_schemas;
use crate::value::VmValue;

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
///
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
        .map(|schema| schema.name)
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
        let (close_start, close_end, calls_before_close) = pair[1];
        if calls_before_close > calls_before_open {
            call_ranges.push((open_start, open_end));
            call_ranges.push((close_start, close_end));
        }
    }
    // Handle a trailing unclosed fence.
    if fence_lines.len() % 2 == 1 {
        let (start, end, calls_before) = *fence_lines.last().unwrap();
        if calls.len() > calls_before {
            call_ranges.push((start, end));
        }
    }
    call_ranges.sort_by_key(|range| range.0);

    // Strip empty fence pairs: models emit them as failed tool-call
    // attempts and they cause duplication loops in conversation history.
    for pair in fence_lines.windows(2) {
        let (open_start, _open_end, calls_before_open) = pair[0];
        let (_close_start, close_end, calls_before_close) = pair[1];
        if calls_before_close == calls_before_open {
            call_ranges.push((open_start, close_end));
        }
    }
    call_ranges.sort_by_key(|range| range.0);
    call_ranges.dedup_by(|right, left| left.0 == right.0);

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
