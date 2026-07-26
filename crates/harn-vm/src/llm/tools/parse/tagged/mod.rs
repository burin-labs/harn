use std::collections::BTreeSet;

use super::super::{
    assistant_prose_block, text_tool_call_block, TEXT_TOOL_CALL_TAG, TEXT_TOOL_CALL_TAG_COMPACT,
};
use super::bare::parse_bare_calls_in_body;
use super::harmony::{
    consume_corrupted_tool_call_close as consume_harmony_corrupted_tool_call_close,
    consume_corrupted_wrapper_open as consume_harmony_corrupted_wrapper_open,
    consume_frame_marker as consume_harmony_frame_marker,
    consume_message_marker_after_tool_call_header,
    consume_tool_call_line as consume_harmony_tool_call_line,
    is_corrupted_tool_call_close_fragment,
};
use super::reserved::recover_malformed_call_opener;
use super::syntax::{
    collapse_blank_lines, find_close_tag, ident_length, match_block, match_tool_call_block,
    parse_ts_call_from, preview_str, render_canonical_call, skip_heredoc_body, strip_thinking_tags,
    CloseScan,
};
use super::TextToolParseResult;
use crate::llm::tools::collect_tool_schemas;
use crate::text_index::TextIndex;
use crate::value::VmValue;

mod function_markup;
mod html_entities;
mod provider_dialects;

use function_markup::{
    parse_function_markup_body, try_parse_top_level_function_markup, FUNCTION_MARKUP_OPEN,
    INVOKE_MARKUP_OPEN,
};
use html_entities::decode_html_entities_in_args;
use provider_dialects::{parse_deepseek_dsml_calls, parse_mistral_marker_calls};

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
/// Harmless top-level narration and recovered bare calls are canonicalized into
/// protocol blocks. Unknown tags and unclosed tags are reported as
/// `violations`; malformed call bodies are reported as `errors` (per-call
/// diagnostics). The function always runs to completion so every actionable
/// violation can be surfaced to the model on the next turn.
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
    if let Some(mut result) = parse_mistral_marker_calls(src, tools_val) {
        assign_turn_unique_ids(&mut result.calls);
        return result;
    }
    if let Some(mut result) = parse_deepseek_dsml_calls(src, tools_val) {
        assign_turn_unique_ids(&mut result.calls);
        return result;
    }

    let mut calls: Vec<serde_json::Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut violations: Vec<String> = Vec::new();
    let mut assistant_prose_parts: Vec<String> = Vec::new();
    let mut user_response_parts: Vec<String> = Vec::new();
    let mut canonical_parts: Vec<String> = Vec::new();
    let mut done_marker: Option<String> = None;
    let mut recovered_from_stray_count = 0usize;

    let mut cursor = 0usize;
    let bytes = src.as_bytes();
    // Fence and line-start positions, resolved once. The scan below asks about
    // them at nearly every position it visits.
    let index = TextIndex::build(src);
    // Byte position just past the most recently consumed top-level block.
    // A tag that follows a consumed block with only whitespace between them
    // is structurally top-level even mid-line: value models chain blocks as
    // `...</tool_call><tool_call>...` on one line, and without this the
    // second open tag fails the line-start check and is shredded into a
    // "stray text" violation (observed at scale in the eval corpus).
    let mut last_block_end = 0usize;

    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            break;
        }
        let adjacent_to_block = last_block_end > 0
            && cursor >= last_block_end
            && src[last_block_end..cursor].chars().all(char::is_whitespace);

        // A `reserved_tool_call_token` model can truncate its `[[CALL]]` wire
        // opener to `[[CALL]` (one bracket short). `wire_to_canonical` only maps
        // the exact `[[CALL]]`, so the stub survives here as literal text and,
        // starting with `[`, would be swept into assistant prose by the non-`<`
        // run handler below — hiding a likely-lost action (harn#4486). Reaching
        // this branch means `cursor` is a genuine top-level position (any
        // `<tool_call>` block, other protocol block, heredoc body, or bare-call
        // run has already been consumed as a unit), so a `[[CALL]` here is a
        // structural opener, not argument/heredoc/prose text.
        if let Some(after) = recover_malformed_call_opener(
            &index,
            src,
            cursor,
            tools_val,
            &mut calls,
            &mut errors,
            &mut canonical_parts,
        ) {
            cursor = after;
            last_block_end = after;
            continue;
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
                if let Some(after) =
                    consume_message_marker_after_tool_call_header(src, start, cursor)
                {
                    cursor = after;
                    continue;
                }
                break;
            }
            let mut stray = StrayReportContext {
                errors: &mut errors,
                violations: &mut violations,
                calls: &mut calls,
                assistant_prose_parts: &mut assistant_prose_parts,
                user_response_parts: &mut user_response_parts,
                canonical_parts: &mut canonical_parts,
                done_marker: &mut done_marker,
                recovered_from_stray_count: &mut recovered_from_stray_count,
            };
            report_stray(&src[start..cursor], tools_val, &mut stray);
            continue;
        }

        if let Some(after) = consume_harmony_tool_call_line(src, cursor) {
            let mut stray = StrayReportContext {
                errors: &mut errors,
                violations: &mut violations,
                calls: &mut calls,
                assistant_prose_parts: &mut assistant_prose_parts,
                user_response_parts: &mut user_response_parts,
                canonical_parts: &mut canonical_parts,
                done_marker: &mut done_marker,
                recovered_from_stray_count: &mut recovered_from_stray_count,
            };
            report_stray(&src[cursor..after], tools_val, &mut stray);
            cursor = after;
            last_block_end = cursor;
            continue;
        }

        if let Some(after) = consume_harmony_corrupted_tool_call_close(src, cursor) {
            cursor = after;
            last_block_end = cursor;
            continue;
        }

        if let Some(after) = consume_harmony_corrupted_wrapper_open(src, cursor) {
            cursor = after;
            last_block_end = cursor;
            continue;
        }

        if let Some(after) = consume_harmony_frame_marker(src, cursor) {
            cursor = after;
            last_block_end = cursor;
            continue;
        }

        if (!adjacent_to_block && !index.is_line_leading(src, cursor))
            || index.inside_markdown_fence(cursor)
        {
            let start = cursor;
            while cursor < bytes.len() && bytes[cursor] != b'\n' {
                cursor += 1;
            }
            let mut stray = StrayReportContext {
                errors: &mut errors,
                violations: &mut violations,
                calls: &mut calls,
                assistant_prose_parts: &mut assistant_prose_parts,
                user_response_parts: &mut user_response_parts,
                canonical_parts: &mut canonical_parts,
                done_marker: &mut done_marker,
                recovered_from_stray_count: &mut recovered_from_stray_count,
            };
            report_stray(&src[start..cursor], tools_val, &mut stray);
            continue;
        }

        if let Some((body, after)) = match_tool_call_block(src, cursor, TEXT_TOOL_CALL_TAG)
            .or_else(|| match_tool_call_block(src, cursor, TEXT_TOOL_CALL_TAG_COMPACT))
        {
            // Weak value models (DeepSeek) wrap THINKING/narration in
            // `<assistant_prose>` *inside* `<tool_call>`, sometimes alongside a
            // real call in the same wrapper. When the body opens with a known
            // narration tag, peel the narration out as prose and recover any
            // real call that follows — never report the narration as a parse
            // error (which would waste the turn telling the model it erred).
            if let Some(narration) = recover_tool_call_narration(body, tools_val) {
                for prose in &narration.prose {
                    assistant_prose_parts.push(prose.clone());
                    canonical_parts.push(assistant_prose_block(prose));
                }
                if let Some(call) = narration.call {
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
                cursor = after;
                last_block_end = after;
                continue;
            }
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
            last_block_end = after;
        } else if let Some(open_len) = unclosed_tool_call_open(src, cursor) {
            // A `<tool_call>` open tag with no matching `</tool_call>` anywhere
            // ahead. This is the signature of an output truncated mid-tool-call
            // — typically the model hit its `max_tokens` cap while emitting a
            // large argument (e.g. a multi-hundred-line `edit({ content: … })`).
            // Without this branch the open tag falls through to the generic
            // "unknown top-level tag" path and the call body becomes "stray
            // text", so the whole turn parses to zero tool calls and the agent
            // loop silently stalls as if the model had only produced prose.
            //
            // Surface it as an actionable `error` (and recover the partial call
            // name when possible) so the loop sees a truncated-tool-call signal
            // rather than an empty text turn.
            let body = &src[cursor + open_len..];
            // Before declaring truncation, try the nested-XML recovery: weak
            // value models open `<tool_call>`, emit `<look>{ ... }`, then close
            // with a mismatched `</look_call>` (or no inner close) and omit the
            // `</tool_call>` entirely. The JSON object is complete, so the call
            // is recoverable rather than truncated — canonicalize and dispatch
            // it exactly like a well-formed block.
            match parse_xml_wrapped_json_args_body(body, tools_val) {
                Ok(Some(call)) => {
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
                    cursor = bytes.len();
                    continue;
                }
                // A recognizable nested-XML shape whose tool is unknown or whose
                // body is malformed: surface the actionable parse error rather
                // than the misleading "truncated" diagnostic, and stop scanning.
                Err(msg) => {
                    errors.push(msg);
                    cursor = bytes.len();
                    continue;
                }
                // Not a nested-XML shape at all — fall through to the
                // truncation diagnostic below.
                Ok(None) => {}
            }
            // Chat-template function markup with an unclosed wrapper (#3220):
            // `<tool_call>` + `<function=edit>...` where the model never
            // emitted `</tool_call>`. Complete parameter blocks make the call
            // recoverable; a truncated parameter surfaces its precise error.
            match parse_function_markup_body(body, tools_val) {
                Ok(Some(call)) => {
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
                    cursor = bytes.len();
                    continue;
                }
                Err(msg) => {
                    errors.push(msg);
                    cursor = bytes.len();
                    continue;
                }
                Ok(None) => {}
            }
            // Canonical bare-call body with an unclosed wrapper (#A2a): the model
            // emitted a structurally COMPLETE `name({ ... <<EOF ... EOF })` but
            // omitted the redundant `</tool_call>` close tag. `stop_reason` is
            // `stop`, not `length` — nothing was truncated, the close tag is just
            // absent. The bare-call parser is heredoc-aware, so it only yields a
            // call when the body is genuinely complete (heredoc sentinel-closed,
            // the call's `)` balanced); a body cut off mid-argument yields no call
            // (or a parse error) and falls through to the TRUNCATED diagnostic.
            if let Some(call) = recover_complete_bare_call_body(body, tools_val) {
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
                cursor = bytes.len();
                continue;
            }
            let recovered_name = leading_call_name(body, tools_val);
            match recovered_name {
                Some(name) => errors.push(format!(
                    "TOOL CALL TRUNCATED: `<tool_call>` opening `{name}(...)` was never \
                     closed — the response appears to have been cut off mid-call (likely \
                     the model hit its max output token limit). Re-emit the complete \
                     `<tool_call>{name}({{ ... }})</tool_call>` block; for very large \
                     arguments, split the work into smaller calls."
                )),
                None => errors.push(
                    "TOOL CALL TRUNCATED: a `<tool_call>` block was opened but never \
                     closed — the response appears to have been cut off (likely the model \
                     hit its max output token limit). Re-emit the complete \
                     `<tool_call>name({ ... })</tool_call>` block, splitting very large \
                     arguments into smaller calls if needed."
                        .to_string(),
                ),
            }
            // Consume the rest of the stream: everything after a truncated open
            // tag is the unterminated call body, not further top-level blocks.
            cursor = bytes.len();
        } else if let Some((body, after)) = match_block(src, cursor, "assistant_prose")
            .or_else(|| match_block(src, cursor, "assistantprose"))
        {
            let trimmed = body.trim();
            if !trimmed.is_empty() {
                assistant_prose_parts.push(trimmed.to_string());
                canonical_parts.push(assistant_prose_block(trimmed));
            }
            cursor = after;
            last_block_end = after;
        } else if let Some((body, after)) = match_block(src, cursor, "user_response")
            .or_else(|| match_block(src, cursor, "userresponse"))
        {
            let trimmed = body.trim();
            if !trimmed.is_empty() {
                user_response_parts.push(trimmed.to_string());
                canonical_parts.push(format!("<user_response>\n{trimmed}\n</user_response>"));
            }
            cursor = after;
            last_block_end = after;
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
            last_block_end = after;
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
            last_block_end = after_call;
        } else if let Some(outcome) = try_parse_top_level_function_markup(src, cursor, tools_val) {
            // Chat-template function markup with no `<tool_call>` wrapper
            // (#3220): `<function=edit><parameter=...>...</parameter></function>`
            // or `<invoke name="edit">...</invoke>` emitted as plain text.
            // Line anchoring and the markdown-fence guard above keep prose
            // mentioning the syntax and fenced examples out of this branch.
            match outcome {
                Ok((call, after)) => {
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
                    violations.push(format!(
                        "Tool call `{name}` was emitted as chat-template function markup \
                         instead of `<tool_call>{name}({{ ... }})</tool_call>`. Recovered \
                         this turn so work moves forward; emit the canonical form on \
                         subsequent turns."
                    ));
                    cursor = after;
                    last_block_end = after;
                }
                Err((msg, after)) => {
                    errors.push(msg);
                    cursor = after;
                    last_block_end = after;
                }
            }
        } else if let Some(skip) = stray_tool_call_close_len(src, cursor) {
            // A bare, orphaned `</tool_call>` (or `</toolcall>`). Weak value
            // models duplicate the close after a recovered nested-XML body
            // (`...</look></tool_call></tool_call>`). It carries no content and
            // no work, so swallow it silently rather than raising a noisy
            // "unknown top-level tag" violation.
            cursor += skip;
            last_block_end = cursor;
        } else if let Some(skip) = function_calls_wrapper_len(src, cursor) {
            // `<function_calls>` / `</function_calls>` — the chat-template
            // wrapper vocabulary some templates emit around `<invoke ...>`
            // markup. The inner `<invoke>` block is parsed by the markup path
            // above; the wrapper tags themselves carry no content, so swallow
            // them silently instead of raising two "unknown top-level tag"
            // violations around an otherwise-recovered call.
            cursor += skip;
            last_block_end = cursor;
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
            if let Some(tag) = known_top_level_open_tag(fragment) {
                // An unclosed `<user_response>` / `<assistant_prose>` whose
                // remainder carries no other block is the terminal-answer
                // shape: the model wrote its final prose, ended the turn, and
                // simply omitted the redundant close tag (observed at scale in
                // the eval corpus, where the whole turn was then rejected as a
                // parse failure). The text is complete — accept it as the
                // block's body instead of killing the answer. A remainder that
                // still contains another top-level block keeps the strict
                // violation: swallowing to EOF there would eat real calls.
                if matches!(tag, "assistant_prose" | "user_response")
                    && !remainder_has_top_level_block(&src[end..])
                {
                    let body = src[end..].trim();
                    if !body.is_empty() {
                        if tag == "user_response" {
                            user_response_parts.push(body.to_string());
                            canonical_parts
                                .push(format!("<user_response>\n{body}\n</user_response>"));
                        } else {
                            assistant_prose_parts.push(body.to_string());
                            canonical_parts.push(assistant_prose_block(body));
                        }
                    }
                    cursor = bytes.len();
                    continue;
                }
                violations.push(format!(
                    "Unclosed <{tag}> block. Close it with </{tag}> or remove it; only \
                     <tool_call>, <assistant_prose>, <user_response>, and <done> are accepted.",
                ));
            } else if fragment.starts_with('<') && !fragment.contains('>') {
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

    if calls.is_empty()
        && user_response_parts.is_empty()
        && done_marker.is_none()
        && !violations.is_empty()
        && !assistant_prose_parts.is_empty()
    {
        assistant_prose_parts.clear();
        canonical_parts.clear();
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

    assign_turn_unique_ids(&mut calls);
    TextToolParseResult {
        calls,
        errors,
        prose: user_response
            .clone()
            .unwrap_or_else(|| assistant_prose_parts.join("\n\n")),
        user_response,
        violations,
        recovered_from_stray_count,
        done_marker,
        canonical: canonical_parts.join("\n\n"),
    }
}

fn known_top_level_open_tag(fragment: &str) -> Option<&'static str> {
    let name = fragment
        .trim()
        .strip_prefix('<')?
        .strip_suffix('>')?
        .split_whitespace()
        .next()
        .unwrap_or("");
    accepted_response_tag_name(name)
}

fn accepted_response_tag_name(name: &str) -> Option<&'static str> {
    match name {
        "tool_call" | "toolcall" => Some("tool_call"),
        "assistant_prose" | "assistantprose" => Some("assistant_prose"),
        "user_response" | "userresponse" => Some("user_response"),
        "done" => Some("done"),
        _ => None,
    }
}

/// Normalize ids within this parser invocation so multiple calls in one model
/// body do not collide. The host primitive restamps ids at the execution seam,
/// where the owning agent session is known.
fn assign_turn_unique_ids(calls: &mut [serde_json::Value]) {
    for (idx, call) in calls.iter_mut().enumerate() {
        if let Some(obj) = call.as_object_mut() {
            obj.insert(
                "id".to_string(),
                serde_json::Value::String(format!("tc_{idx}")),
            );
        }
    }
}

pub(super) fn canonical_for_recovered_calls(calls: &[serde_json::Value], prose: &str) -> String {
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

pub(super) fn prose_without_ranges(src: &str, ranges: &[(usize, usize)]) -> String {
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
    let known = known_tool_names_with_implicit(tools_val);
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

/// If `cursor` sits on a `<tool_call>` / `<toolcall>` open tag that has no
/// matching closing tag anywhere ahead in `src`, return the open tag's byte
/// length. `None` when the cursor isn't on a tool-call open tag, or when the
/// block is properly closed (the normal `match_block` path handles that).
///
/// This detects an output truncated mid-tool-call (the model hit its
/// `max_tokens` cap while streaming a large argument), so the caller can emit
/// a precise diagnostic instead of shredding the partial call into stray text.
pub(super) fn unclosed_tool_call_open(src: &str, cursor: usize) -> Option<usize> {
    let rest = &src[cursor..];
    let (open, close) = if rest.starts_with(&format!("<{TEXT_TOOL_CALL_TAG}>")) {
        (
            format!("<{TEXT_TOOL_CALL_TAG}>"),
            format!("</{TEXT_TOOL_CALL_TAG}>"),
        )
    } else if rest.starts_with(&format!("<{TEXT_TOOL_CALL_TAG_COMPACT}>")) {
        (
            format!("<{TEXT_TOOL_CALL_TAG_COMPACT}>"),
            format!("</{TEXT_TOOL_CALL_TAG_COMPACT}>"),
        )
    } else {
        return None;
    };
    // A `</tool_call>` buried in a heredoc body is not a real close, so scan
    // heredoc-aware: only a `Found` close means the block is properly
    // terminated. `NeedMore` (truncated mid-heredoc) and `NotFound` both mean
    // the open tag was never closed.
    if matches!(
        find_close_tag(src, cursor + open.len(), &close),
        CloseScan::Found(_)
    ) {
        return None;
    }
    Some(open.len())
}

/// If `cursor` sits on a bare closing `</tool_call>` / `</toolcall>` tag (no
/// matching open consumed it), return the tag's byte length so the scanner can
/// skip it silently. This swallows the duplicate/trailing close tag that weak
/// value models emit after a recovered nested-XML body.
fn stray_tool_call_close_len(src: &str, cursor: usize) -> Option<usize> {
    let rest = &src[cursor..];
    for tag in [TEXT_TOOL_CALL_TAG, TEXT_TOOL_CALL_TAG_COMPACT] {
        let close = format!("</{tag}>");
        if rest.starts_with(&close) {
            return Some(close.len());
        }
    }
    None
}

/// If `cursor` sits on a bare `<function_calls>` / `</function_calls>` wrapper
/// tag — the chat-template vocabulary some templates emit around `<invoke ...>`
/// markup — return the tag's byte length so the scanner can skip it silently.
/// The wrapper carries no content of its own; the inner `<invoke>` block is
/// handled by the function-markup path.
fn function_calls_wrapper_len(src: &str, cursor: usize) -> Option<usize> {
    let rest = &src[cursor..];
    for tag in ["<function_calls>", "</function_calls>"] {
        if rest.starts_with(tag) {
            return Some(tag.len());
        }
    }
    None
}

/// True when `remainder` still contains another top-level block opener — a
/// `<tool_call>`, `<done>`, a response tag, or chat-template function markup.
/// Used to gate the unclosed-terminal-response recovery: only a remainder with
/// NO further block may be absorbed as the unclosed tag's body.
fn remainder_has_top_level_block(remainder: &str) -> bool {
    const BLOCK_OPENERS: &[&str] = &[
        "<tool_call>",
        "<toolcall>",
        "<done>",
        "<assistant_prose>",
        "<assistantprose>",
        "<user_response>",
        "<userresponse>",
        FUNCTION_MARKUP_OPEN,
        INVOKE_MARKUP_OPEN,
    ];
    BLOCK_OPENERS
        .iter()
        .any(|opener| remainder.contains(opener))
}

/// Recover a single COMPLETE bare `name({ ... })` call from the body of an
/// unclosed `<tool_call>` wrapper (#A2a). Value models emit a structurally
/// complete call — heredoc sentinel-closed, the call's `)` balanced — and
/// simply omit the redundant `</tool_call>` close tag (observed live across
/// swift/zig/scala on fw-gpt-oss-120b with `stop_reason: stop`, i.e. the model
/// finished its turn rather than hitting the output cap). Without this the body
/// falls through to the "TOOL CALL TRUNCATED" diagnostic and the generated file
/// is discarded.
///
/// `parse_bare_calls_in_body` is heredoc-aware, so it yields a call ONLY when
/// the body is genuinely complete: a body cut off mid-heredoc or before the
/// call's closing `)` parses to zero calls (or a parse error), so the caller
/// correctly falls through to the truncation diagnostic in that case.
///
/// Returns `Some(call)` only when EXACTLY ONE well-formed call is present and
/// no parse error was raised — so a malformed-but-recognizable body still
/// surfaces its real diagnostic via the existing fall-through path, and a body
/// carrying multiple bare calls is not silently collapsed to one.
fn recover_complete_bare_call_body(
    body: &str,
    tools_val: Option<&VmValue>,
) -> Option<serde_json::Value> {
    let inner = parse_bare_calls_in_body(body, tools_val);
    if !inner.errors.is_empty() || inner.calls.len() != 1 {
        return None;
    }
    inner.calls.into_iter().next()
}

/// Best-effort recovery of the tool name from a truncated `<tool_call>` body:
/// the leading `name(` of an unterminated call, when `name` is a registered
/// tool. Used only for a clearer truncation diagnostic — never to dispatch.
pub(super) fn leading_call_name(body: &str, tools_val: Option<&VmValue>) -> Option<String> {
    let trimmed = body.trim_start();
    let name_len = ident_length(trimmed.as_bytes())?;
    if name_len == 0 || trimmed.as_bytes().get(name_len) != Some(&b'(') {
        return None;
    }
    let name = &trimmed[..name_len];
    let known = known_tool_names_with_implicit(tools_val);
    known.contains(name).then(|| name.to_string())
}

/// Report stray text that sits outside any recognized top-level tag.
/// When the stray content contains parseable tool calls, execute them by
/// routing them through the canonical-call path. Canonical replay carries the
/// wrapper signal; a separate soft violation would burn a paid turn even though
/// useful calls were already dispatched.
struct StrayReportContext<'a> {
    errors: &'a mut Vec<String>,
    violations: &'a mut Vec<String>,
    calls: &'a mut Vec<serde_json::Value>,
    assistant_prose_parts: &'a mut Vec<String>,
    user_response_parts: &'a mut Vec<String>,
    canonical_parts: &'a mut Vec<String>,
    done_marker: &'a mut Option<String>,
    recovered_from_stray_count: &'a mut usize,
}

fn report_stray(fragment: &str, tools_val: Option<&VmValue>, ctx: &mut StrayReportContext<'_>) {
    let trimmed = fragment.trim();
    if trimmed.is_empty() {
        return;
    }
    if is_orphaned_tool_call_wrapper_fragment(trimmed) {
        return;
    }
    let sniff = parse_bare_calls_in_body(trimmed, tools_val);
    if !sniff.calls.is_empty() {
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
            ctx.canonical_parts
                .push(text_tool_call_block(&render_canonical_call(&name, &args)));
            ctx.calls.push(call.clone());
        }
        *ctx.recovered_from_stray_count += sniff.calls.len();
        let prose = sniff.prose.trim();
        if !prose.is_empty() && should_salvage_stray_prose(prose) {
            push_assistant_prose(prose, ctx.assistant_prose_parts, ctx.canonical_parts);
        }
    } else if !sniff.errors.is_empty() {
        ctx.errors.extend(sniff.errors);
    } else if !salvage_stray_done(
        trimmed,
        ctx.user_response_parts,
        ctx.canonical_parts,
        ctx.done_marker,
    ) {
        if should_salvage_stray_prose(trimmed) {
            push_assistant_prose(trimmed, ctx.assistant_prose_parts, ctx.canonical_parts);
        } else {
            ctx.violations.push(format!(
                "Stray text outside response tags: {:?}. Wrap all prose in \
                 <assistant_prose>...</assistant_prose> or <user_response>...</user_response>, \
                 and every tool call in <tool_call>...</tool_call>.",
                preview_str(trimmed, 120)
            ));
        }
    }
}

fn is_orphaned_tool_call_wrapper_fragment(trimmed: &str) -> bool {
    trimmed == "<tool_call>"
        || trimmed == "</tool_call>"
        || is_corrupted_tool_call_close_fragment(trimmed)
}

fn push_assistant_prose(
    body: &str,
    assistant_prose_parts: &mut Vec<String>,
    canonical_parts: &mut Vec<String>,
) {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return;
    }
    assistant_prose_parts.push(trimmed.to_string());
    canonical_parts.push(assistant_prose_block(trimmed));
}

fn push_user_response(
    body: &str,
    user_response_parts: &mut Vec<String>,
    canonical_parts: &mut Vec<String>,
) {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return;
    }
    user_response_parts.push(trimmed.to_string());
    canonical_parts.push(format!("<user_response>\n{trimmed}\n</user_response>"));
}

fn salvage_stray_done(
    body: &str,
    user_response_parts: &mut Vec<String>,
    canonical_parts: &mut Vec<String>,
    done_marker: &mut Option<String>,
) -> bool {
    const DEFAULT_DONE_SENTINEL: &str = "##DONE##";
    let Some((before, after)) = body.split_once(DEFAULT_DONE_SENTINEL) else {
        return false;
    };
    if !after.trim().is_empty() {
        return false;
    }
    push_user_response(before, user_response_parts, canonical_parts);
    *done_marker = Some(DEFAULT_DONE_SENTINEL.to_string());
    canonical_parts.push(format!("<done>{DEFAULT_DONE_SENTINEL}</done>"));
    true
}

fn should_salvage_stray_prose(body: &str) -> bool {
    !has_response_protocol_fragment(body)
}

fn has_response_protocol_fragment(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    if lower.contains("_call>") {
        return true;
    }
    let mut rest = lower.as_str();
    while let Some(start) = rest.find('<') {
        let candidate = &rest[start..];
        let end = candidate
            .find('>')
            .map(|offset| offset + 1)
            .unwrap_or(candidate.len());
        if response_protocol_fragment_tag(&candidate[..end]).is_some() {
            return true;
        }
        if end >= candidate.len() {
            break;
        }
        rest = &candidate[end..];
    }
    false
}

fn response_protocol_fragment_tag(fragment: &str) -> Option<&'static str> {
    let inner = fragment.trim().strip_prefix('<')?;
    let inner = inner.strip_prefix('/').unwrap_or(inner).trim_start();
    let inner = inner.strip_suffix('>').unwrap_or(inner);
    let name = inner.split_whitespace().next().unwrap_or("");
    accepted_response_tag_name(name)
}

/// Parse a single `<tool_call>` body. Expects exactly one bare
/// `name({ ... })` expression (possibly with surrounding whitespace).
pub(super) fn parse_single_tool_call(
    body: &str,
    tools_val: Option<&VmValue>,
) -> Result<serde_json::Value, String> {
    if let Some(call) = parse_json_tool_call_body(body, tools_val)? {
        return Ok(call);
    }
    if let Some(call) = parse_xml_wrapped_json_args_body(body, tools_val)? {
        return Ok(call);
    }
    // Chat-template function markup inside the wrapper (#3220):
    // `<tool_call><function=edit><parameter=action>...</parameter></function></tool_call>`.
    if let Some(call) = parse_function_markup_body(body, tools_val)? {
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

/// Narration recovered from a `<tool_call>` wrapper that the body parser
/// rejected: the assistant's prose plus any real call that shared the wrapper.
struct ToolCallNarration {
    prose: Vec<String>,
    call: Option<serde_json::Value>,
}

/// Known *narration* tags a value model may wrap inside `<tool_call>` while it
/// is only thinking out loud. Deliberately tiny and allowlisted: a narration
/// tag is special precisely because it is NOT an attempted tool invocation, so
/// it must never widen to cover unknown tags that look like calls (e.g.
/// `<frobnicate>{...}</frobnicate>` stays a rejected unknown tool). Compact
/// (tagless-underscore) spellings mirror the top-level `<assistant_prose>` /
/// `<user_response>` aliases the parser already accepts.
const NARRATION_TAGS: &[&str] = &["assistant_prose", "assistantprose", "thinking", "reasoning"];

/// Reclassify a `<tool_call>` body that failed to parse as a call. Returns
/// `Some` only when the body is narration the model mis-wrapped in tool-call
/// tags, in which case the surrounding turn should NOT be reported as a parse
/// error:
///
/// * The body opens with a known narration tag (`<assistant_prose>…`). Peel off
///   every leading narration block as prose; if a real `name({ ... })` / nested
///   tool call follows in the same wrapper, recover it too.
/// * The body is bare prose — no inner `<tag>` and no recoverable call (e.g.
///   `<tool_call>Reading the file.</tool_call>`). Treat the text as narration.
///
/// Returns `None` for anything that looks like an attempted (but malformed or
/// unknown) tool call so the caller surfaces the existing actionable error and
/// the #3132/6b970d61 unknown-tag discipline is preserved.
fn recover_tool_call_narration(
    body: &str,
    tools_val: Option<&VmValue>,
) -> Option<ToolCallNarration> {
    let mut prose: Vec<String> = Vec::new();
    let mut rest = body.trim();

    // Peel leading narration blocks. `match_block` finds the matching close
    // tag, so a narration block followed by a real call is split cleanly.
    loop {
        let opened = NARRATION_TAGS
            .iter()
            .find_map(|tag| match_block(rest, 0, tag).map(|matched| (tag, matched)));
        match opened {
            Some((_, (inner, after))) => {
                let trimmed = inner.trim();
                if !trimmed.is_empty() {
                    prose.push(trimmed.to_string());
                }
                rest = rest[after..].trim_start();
            }
            None => break,
        }
    }

    let remainder = rest.trim();
    if !prose.is_empty() {
        // Narration tag(s) present. Recover a real call from whatever follows,
        // if any; ignore a parse failure on the remainder (it is trailing slop,
        // not the model's intended action — the prose already carries the turn).
        let call = if remainder.is_empty() {
            None
        } else {
            parse_single_tool_call(remainder, tools_val).ok()
        };
        return Some(ToolCallNarration { prose, call });
    }

    // No narration tag. Only reclassify as prose when the body is plainly NOT
    // an attempted tool call: it carries no inner `<tag>` (not an unknown-tool
    // attempt like `<frobnicate>{...}`), no `{` JSON body (the Gemma-style
    // `{ "name": …, "arguments": … }` shape), no `[` JSON-array body (a
    // `[{ "name": …, "arguments": … }]` call the model wrapped in a one- or
    // many-element array — `parse_json_tool_call_body` dispatches a single
    // element and gives an actionable "one call per block" error otherwise;
    // without this guard the whole array was silently swallowed as prose and
    // the call vanished with no feedback), and no `name(` call head (so a
    // bare call to an unknown/misspelled tool still surfaces its real error
    // instead of being silently swallowed as prose).
    if remainder.starts_with('<')
        || remainder.starts_with('{')
        || remainder.starts_with('[')
        || remainder.is_empty()
        || looks_like_call_head(remainder)
    {
        return None;
    }
    Some(ToolCallNarration {
        prose: vec![remainder.to_string()],
        call: None,
    })
}

/// True if `src` opens with a `name(` token — the head of a (possibly
/// unknown-tool) call. Used to keep an attempted bare call out of the
/// narration path so its real parse error still surfaces.
fn looks_like_call_head(src: &str) -> bool {
    let bytes = src.as_bytes();
    let Some(name_len) = ident_length(bytes) else {
        return false;
    };
    let mut idx = name_len;
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    bytes.get(idx) == Some(&b'(')
}

pub(super) fn known_tool_names_with_implicit(tools_val: Option<&VmValue>) -> BTreeSet<String> {
    collect_tool_schemas(tools_val, None)
        .into_iter()
        .map(|schema| schema.name)
        .chain(["ledger".to_string(), "load_skill".to_string()])
        .collect()
}

/// Recover a nested XML function wrapper inside a `<tool_call>` body, e.g.
/// `<edit>{ ... }</edit>`. Tolerates the sloppy shapes weak value models emit:
/// a mismatched inner close tag (`</edit_call>`), a missing inner close tag, or
/// a duplicate/trailing `</tool_call>` after the JSON object. The inner tag must
/// name a registered/implicit tool and be followed by a JSON object, otherwise
/// the body falls through to the bare-call path and unknown tags are rejected.
fn parse_xml_wrapped_json_args_body(
    body: &str,
    tools_val: Option<&VmValue>,
) -> Result<Option<serde_json::Value>, String> {
    let trimmed = body.trim();
    let bytes = trimmed.as_bytes();
    if bytes.first() != Some(&b'<') {
        return Ok(None);
    }
    let name_start = 1usize;
    let Some(name_len) = ident_length(&bytes[name_start..]) else {
        return Ok(None);
    };
    let name_end = name_start + name_len;
    if bytes.get(name_end) != Some(&b'>') {
        return Ok(None);
    }
    let name = &trimmed[name_start..name_end];
    // The JSON body starts after the inner open tag. Anything after the JSON
    // object's closing brace (a matched `</edit>`, a mismatched `</edit_call>`,
    // a stray `</tool_call>`, or nothing) is tolerated trailing slop.
    let after_open = trimmed[name_end + 1..].trim_start();
    if !after_open.starts_with('{') {
        // Not a JSON-object body — let the bare-call path handle (or reject) it.
        return Ok(None);
    }
    let known = known_tool_names_with_implicit(tools_val);
    if !known.contains(name) {
        let available: Vec<_> = known.iter().take(20).cloned().collect();
        return Err(format!(
            "Unknown tool '{}' in nested XML tool-call body. Available tools: [{}]",
            name,
            available.join(", ")
        ));
    }
    let Some(obj_len) = balanced_json_object_len(after_open) else {
        return Err(format!(
            "<tool_call><{name}> body did not contain a complete JSON object. \
             Emit `<tool_call>{name}({{ ... }})</tool_call>` instead."
        ));
    };
    let json_src = &after_open[..obj_len];
    let mut arguments: serde_json::Value = serde_json::from_str(json_src).map_err(|error| {
        format!(
            "<tool_call><{name}> body did not parse as a JSON object: {error}. \
             Emit `<tool_call>{name}({{ ... }})</tool_call>` instead."
        )
    })?;
    if !arguments.is_object() {
        return Err(format!(
            "Nested XML arguments for tool '{name}' must be a JSON object, got `{arguments}`."
        ));
    }
    // Same JSON-string channel as `parse_json_tool_call_body`: decode escaped
    // markup delimiters exactly once so operators are not shipped HTML-encoded.
    decode_html_entities_in_args(&mut arguments);
    Ok(Some(serde_json::json!({
        "id": format!("tc_xml_{name}"),
        "name": name,
        "arguments": arguments,
    })))
}

/// Byte length of the first balanced `{ ... }` JSON object at the start of
/// `src` (which must begin with `{`), brace-counting while skipping string
/// spans so braces inside string values don't miscount. Returns `None` if the
/// object is never closed.
pub(super) fn balanced_json_object_len(src: &str) -> Option<usize> {
    let bytes = src.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, &byte) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx + 1);
                }
            }
            _ => {}
        }
    }
    None
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
    let known = known_tool_names_with_implicit(tools_val);
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
    let mut arguments = match arguments {
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
    // The value arrived through the JSON-string channel, where a text-format
    // model escapes its markup delimiters (`&lt;`, `=&gt;`, `&amp;&amp;`).
    // Decode those references exactly once here so operators reach the tool as
    // real source, never HTML-encoded bytes that cannot compile.
    decode_html_entities_in_args(&mut arguments);
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
