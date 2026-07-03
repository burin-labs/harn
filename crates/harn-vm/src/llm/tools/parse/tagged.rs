use std::collections::{BTreeMap, BTreeSet};

use super::super::type_expr::TypeExpr;
use super::super::{text_tool_call_block, TEXT_TOOL_CALL_TAG, TEXT_TOOL_CALL_TAG_COMPACT};
use super::bare::parse_bare_calls_in_body;
use super::syntax::{
    collapse_blank_lines, find_close_tag, ident_length, match_block, match_tool_call_block,
    parse_ts_call_from, preview_str, render_canonical_call, skip_heredoc_body, strip_thinking_tags,
    CloseScan,
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

    let mut cursor = 0usize;
    let bytes = src.as_bytes();
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

        if (!adjacent_to_block && !is_top_level_tag_position(src, cursor))
            || inside_markdown_fence(src, cursor)
        {
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

        if src[cursor..].starts_with("<|") {
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
                    canonical_parts.push(format!("<assistant_prose>\n{prose}\n</assistant_prose>"));
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
                canonical_parts.push(format!("<assistant_prose>\n{trimmed}\n</assistant_prose>"));
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
                            canonical_parts
                                .push(format!("<assistant_prose>\n{body}\n</assistant_prose>"));
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
    match name {
        "tool_call" | "toolcall" => Some("tool_call"),
        "assistant_prose" | "assistantprose" => Some("assistant_prose"),
        "user_response" | "userresponse" => Some("user_response"),
        "done" => Some("done"),
        _ => None,
    }
}

/// Overwrite every parsed call's `id` with a turn-unique `tc_{n}`. The per-body
/// parsers mint ids against their *local* call vector, so a body with a single
/// call always gets `tc_0` and the JSON path hard-codes `tc_json` — meaning two
/// `<tool_call>` blocks in one turn collide on `tc_0` and their results can't be
/// correlated in the run-event stream. This is the one place with the
/// turn-global index, so it owns final id assignment (mirroring the streaming
/// detector's globally-unique `text-cand-{seq}` ids).
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
        done_marker: None,
        canonical,
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
    let block_re = regex::Regex::new(r"(?s)<｜DSML｜function_calls>.*?</｜DSML｜function_calls>")
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
fn unclosed_tool_call_open(src: &str, cursor: usize) -> Option<usize> {
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
fn leading_call_name(body: &str, tools_val: Option<&VmValue>) -> Option<String> {
    let trimmed = body.trim_start();
    let name_len = ident_length(trimmed.as_bytes())?;
    if name_len == 0 || trimmed.as_bytes().get(name_len) != Some(&b'(') {
        return None;
    }
    let name = &trimmed[..name_len];
    let known = known_tool_names_with_implicit(tools_val);
    known.contains(name).then(|| name.to_string())
}

fn is_top_level_tag_position(src: &str, cursor: usize) -> bool {
    let line_start = src[..cursor].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    src[line_start..cursor]
        .chars()
        .all(|ch| matches!(ch, ' ' | '\t' | '\r'))
}

/// Is `cursor` enclosed by a *closed* markdown code fence?
///
/// A top-level tag inside a ```` ```lang … ``` ```` block is narration, not a
/// real block, so the scanner skips it (e.g. an example `<user_response>` shown
/// in fenced docs). But the cursor only counts as "inside a fence" when the
/// opening fence is matched by a closing fence *at or after* the cursor.
///
/// The earlier implementation just counted ```` ``` ```` markers before the
/// cursor and called an odd count "inside a fence". That shredded a legitimate
/// trailing `<tool_call>` whenever an unbalanced ```` ``` ```` appeared earlier
/// in the response: the open fence with no close had no business swallowing a
/// later real block. Requiring a matching close ahead makes an unbalanced
/// *trailing* fence harmless while keeping closed example fences skipped.
fn inside_markdown_fence(src: &str, cursor: usize) -> bool {
    // Walk fence markers left to right, toggling parity. We only care whether
    // the fence that is open *at* the cursor is later closed.
    let mut open_before_cursor = false;
    let mut scan = 0;
    while let Some(rel) = src[scan..].find("```") {
        let pos = scan + rel;
        if pos >= cursor {
            // First fence marker at/after the cursor. If a fence was open when
            // we crossed the cursor, this marker closes it → cursor is inside a
            // real (closed) fence. Otherwise the cursor sat in open prose.
            return open_before_cursor;
        }
        open_before_cursor = !open_before_cursor;
        scan = pos + 3;
    }
    // No fence marker at/after the cursor: any fence still open here is an
    // unbalanced trailing fence, so the cursor is not inside a closed fence.
    false
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

fn known_tool_names_with_implicit(tools_val: Option<&VmValue>) -> BTreeSet<String> {
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
    let arguments: serde_json::Value = serde_json::from_str(json_src).map_err(|error| {
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
fn balanced_json_object_len(src: &str) -> Option<usize> {
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

/// Opening markers for the chat-template "function markup" rendering of a
/// tool call. `<function=NAME>` is the qwen3 chat-template style; `<invoke
/// name="NAME">` is the attribute spelling other templates use.
const FUNCTION_MARKUP_OPEN: &str = "<function=";
const INVOKE_MARKUP_OPEN: &str = "<invoke name=";

/// Parse the chat-template "function markup" rendering of a tool call that
/// native-format models (observed live: qwen3.6 under long context, #3220)
/// sometimes emit as plain assistant text instead of using the provider's
/// structured tool channel:
///
/// ```text
/// <function=edit>
/// <parameter=action>
/// create
/// </parameter>
/// </function>
/// ```
///
/// or the attribute spelling `<invoke name="edit">` +
/// `<parameter name="action">create</parameter>` + `</invoke>`.
///
/// `body` is the markup WITHOUT the optional `<tool_call>` wrapper. Returns:
/// * `Ok(None)` — body does not open with a plausible function-markup tag
///   (the caller falls through to its other recovery paths);
/// * `Ok(Some(call))` — a complete markup block for a registered tool. A
///   missing `</function>` / `</invoke>` close is tolerated as long as every
///   `<parameter ...>` block is itself properly closed (some emissions close
///   only the outer `</tool_call>`);
/// * `Err(msg)` — recognizably function markup but unusable: unknown tool,
///   an unterminated `<parameter ...>` block (truncation — never dispatch a
///   partial argument value), or a second call sharing the block. The message
///   is model-facing parse feedback.
///
/// Parameter values are typed against the registered tool schema: parameters
/// the schema declares as strings (or whose schema entry is missing) keep
/// their raw bytes verbatim — code in an `edit` `content` value is never
/// JSON-mangled — while non-string parameters are parsed as JSON with a
/// fallback to the raw string.
fn parse_function_markup_body(
    body: &str,
    tools_val: Option<&VmValue>,
) -> Result<Option<serde_json::Value>, String> {
    let trimmed = body.trim();
    let (name, after_open, close_tag, style) =
        if let Some(rest) = trimmed.strip_prefix(FUNCTION_MARKUP_OPEN) {
            let Some(gt) = rest.find('>') else {
                return Err(
                    "TOOL CALL TRUNCATED: a `<function=` open tag was never closed with `>` — \
                     the response appears to have been cut off. The call was NOT executed; \
                     re-emit the complete call."
                        .to_string(),
                );
            };
            (
                rest[..gt].trim().trim_matches('"'),
                &rest[gt + 1..],
                "</function>",
                "`<function=...>`",
            )
        } else if let Some(rest) = trimmed.strip_prefix(INVOKE_MARKUP_OPEN) {
            let Some(gt) = rest.find('>') else {
                return Err(
                    "TOOL CALL TRUNCATED: an `<invoke name=` open tag was never closed with \
                     `>` — the response appears to have been cut off. The call was NOT \
                     executed; re-emit the complete call."
                        .to_string(),
                );
            };
            (
                rest[..gt].trim().trim_matches('"'),
                &rest[gt + 1..],
                "</invoke>",
                "`<invoke name=...>`",
            )
        } else {
            return Ok(None);
        };
    // Require a plausible tool identifier; anything else is not tool-call
    // markup (e.g. prose like `<function=a tool of your choice>`), so let the
    // caller's other paths classify it.
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Ok(None);
    }
    let known = known_tool_names_with_implicit(tools_val);
    if !known.contains(name) {
        let available: Vec<_> = known.iter().take(20).cloned().collect();
        return Err(format!(
            "Unknown tool '{name}' in chat-template {style} tool-call markup. \
             Available tools: [{}]",
            available.join(", ")
        ));
    }
    // Anything after the close tag (a stray `</tool_call>`, whitespace) is
    // tolerated slop. A missing close tag is tolerated only when every
    // parameter block below is itself closed — see the leftover check.
    let inner = match after_open.find(close_tag) {
        Some(idx) => &after_open[..idx],
        None => after_open,
    };
    let param_types: BTreeMap<String, TypeExpr> = collect_tool_schemas(tools_val, None)
        .into_iter()
        .find(|schema| schema.name == name)
        .map(|schema| {
            schema
                .params
                .into_iter()
                .map(|param| (param.name, param.ty))
                .collect()
        })
        .unwrap_or_default();
    // The attribute spelling tolerates extra attributes after `name="..."`
    // (e.g. `<parameter name="file" string="true">`, the DSML-influenced shape
    // value models emit): requiring `>` right after the name silently failed
    // the match, and the complete call was then misdiagnosed as TRUNCATED —
    // the single largest full-turn parse-kill class in the eval corpus.
    let param_re = regex::Regex::new(
        r#"(?s)<parameter(?:=([A-Za-z0-9_][A-Za-z0-9_.-]*)|\s+name="([^"]+)"(?:\s+[A-Za-z_][A-Za-z0-9_.-]*="[^"]*")*)\s*>(.*?)</parameter>"#,
    )
    .expect("valid function-markup parameter regex");
    let mut args = serde_json::Map::new();
    let mut covered: Vec<(usize, usize)> = Vec::new();
    for captures in param_re.captures_iter(inner) {
        let whole = captures.get(0).expect("whole parameter match");
        covered.push((whole.start(), whole.end()));
        let key = captures
            .get(1)
            .or_else(|| captures.get(2))
            .map(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let raw = captures.get(3).map(|m| m.as_str()).unwrap_or("");
        let value = function_markup_param_value(raw, param_types.get(&key));
        args.insert(key, value);
    }
    let leftover = prose_without_ranges(inner, &covered);
    if leftover.contains("<parameter") {
        return Err(format!(
            "TOOL CALL TRUNCATED: a `<parameter ...>` block in the {style} markup for \
             `{name}` was never closed with `</parameter>` — the response appears to have \
             been cut off. The call was NOT executed; re-emit the complete call."
        ));
    }
    if leftover.contains(FUNCTION_MARKUP_OPEN) || leftover.contains(INVOKE_MARKUP_OPEN) {
        return Err(format!(
            "The {style} markup block for `{name}` contained more than one call; \
             emit one call per <tool_call> block."
        ));
    }
    Ok(Some(serde_json::json!({
        "id": format!("tc_fnmarkup_{name}"),
        "name": name,
        "arguments": serde_json::Value::Object(args),
    })))
}

/// Type a raw function-markup parameter value against its schema entry.
///
/// The chat-template markup carries no type information — values are raw
/// text framed by the template's own newlines. Strip exactly one leading and
/// one trailing newline (the template's framing, not the value's), then:
/// string-typed (or schema-unknown) parameters keep the framed bytes
/// verbatim; non-string parameters are parsed as JSON, falling back to the
/// raw string when they don't parse.
fn function_markup_param_value(raw: &str, ty: Option<&TypeExpr>) -> serde_json::Value {
    let framed = raw
        .strip_prefix("\r\n")
        .or_else(|| raw.strip_prefix('\n'))
        .unwrap_or(raw);
    let framed = framed
        .strip_suffix("\r\n")
        .or_else(|| framed.strip_suffix('\n'))
        .unwrap_or(framed);
    let wants_string = ty.map(type_expr_wants_string).unwrap_or(true);
    if wants_string {
        return serde_json::Value::String(framed.to_string());
    }
    match serde_json::from_str::<serde_json::Value>(framed.trim()) {
        Ok(value) => value,
        Err(_) => serde_json::Value::String(framed.to_string()),
    }
}

/// True when a schema type only admits string values, so a raw markup value
/// must be kept verbatim rather than JSON-parsed.
fn type_expr_wants_string(ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Primitive(name) => name == "string",
        TypeExpr::Literal(value) => value.is_string(),
        TypeExpr::Union(items) => items.iter().all(type_expr_wants_string),
        _ => false,
    }
}

/// Try to parse top-level chat-template function markup (no `<tool_call>`
/// wrapper) at `cursor`. Returns `None` when the cursor is not on a
/// plausible function-markup opener; otherwise the parsed call or the
/// model-facing diagnostic, each paired with the cursor position after the
/// consumed block. Line anchoring and markdown-fence exclusion are enforced
/// by the caller (the main scanner checks both before dispatching tags), so
/// prose mentioning the syntax inline or fenced examples never reach this.
#[allow(clippy::type_complexity)]
fn try_parse_top_level_function_markup(
    src: &str,
    cursor: usize,
    tools_val: Option<&VmValue>,
) -> Option<Result<(serde_json::Value, usize), (String, usize)>> {
    let rest = &src[cursor..];
    let close_tag = if rest.starts_with(FUNCTION_MARKUP_OPEN) {
        "</function>"
    } else if rest.starts_with(INVOKE_MARKUP_OPEN) {
        "</invoke>"
    } else {
        return None;
    };
    let end = rest
        .find(close_tag)
        .map(|idx| cursor + idx + close_tag.len())
        .unwrap_or(src.len());
    match parse_function_markup_body(&src[cursor..end], tools_val) {
        Ok(Some(call)) => Some(Ok((call, end))),
        Err(msg) => Some(Err((msg, end))),
        Ok(None) => None,
    }
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
