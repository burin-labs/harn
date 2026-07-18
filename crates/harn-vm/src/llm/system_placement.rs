//! Portable placement of interleaved `system`/`developer` messages.
//!
//! A script can put a `{role: "system"}` (or `{role: "developer"}`) message
//! anywhere in the conversation — via the `add_system` builtin,
//! `add_message(convo, "developer", ...)`, or a raw `messages` array — to
//! deliver an operator instruction mid-conversation (a mode switch, a
//! runtime-fetched constraint, injected state). Providers disagree sharply on
//! whether and how a non-leading system message may appear on the wire:
//!
//! - **OpenAI Chat Completions / Responses, Ollama** accept a `system` or
//!   `developer` message at any position.
//! - **Anthropic Fable 5, Mythos 5, and Opus 4.8** accept an interleaved
//!   `system` directive, but only
//!   when it follows a `user` turn (or an assistant turn ending in a server-tool
//!   result), is the last message or is followed by an `assistant` turn, and is
//!   not `messages[0]`. Consecutive directives form one section. Anywhere else
//!   — or on any older/other Claude — the Messages API rejects it with HTTP 400.
//! - **Gemini / Bedrock** have no positional system channel at all; a
//!   `systemInstruction` / `system[]` field is top-level only.
//!
//! Without normalization the script author is fully exposed to those rules: the
//! same conversation works on OpenAI, is silently repositioned to the global
//! system prompt on Gemini/Bedrock, and 400s on Anthropic. This module removes
//! that leak. It runs once, at the `LlmRequestPayload` egress boundary (next to
//! [`apply_thinking_disable_directive`](crate::llm::api), the sibling
//! capability-aware payload post-processor), and rewrites the conversation to
//! the form the target route accepts — driven entirely by the
//! [`SystemMessagePlacement`] capability, never by a hardcoded provider check.
//!
//! The persisted transcript is unchanged: like the Anthropic key-stripping and
//! the `/no_think` directive, this transforms only the send-safe payload.

use serde_json::Value;

use crate::llm::capabilities::{resolve_system_message_placement, SystemMessagePlacement};

/// Roles that carry an operator instruction rather than conversational turns.
fn is_system_or_developer(message: &Value) -> bool {
    matches!(role_of(message), Some("system") | Some("developer"))
}

fn is_user(message: &Value) -> bool {
    role_of(message) == Some("user")
}

fn is_assistant(message: &Value) -> bool {
    role_of(message) == Some("assistant")
}

fn role_of(message: &Value) -> Option<&str> {
    message.get("role").and_then(Value::as_str)
}

/// Render directive content for routes that have no native positional system
/// channel. Text blocks stay readable; other blocks are serialized instead of
/// being silently dropped.
fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|block| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| block.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Wrap directive text in the same `<system-reminder>` envelope the reminder
/// channel uses (see `helpers::options::reminders`), so a folded operator
/// instruction reads identically whether it arrived as a reminder or a
/// `role: "system"` message.
fn system_reminder_block(text: &str) -> String {
    format!(
        "<system-reminder>\n{}\n</system-reminder>",
        crate::stdlib::xml::escape_xml_text(text)
    )
}

/// Append `text` to a message's `content`, preserving whatever shape it already
/// uses (string or content-block array). Mirrors the reminder channel's
/// `append_text_to_message_content` so folded directives land the same way.
fn append_text_to_content(content: &mut Value, text: &str) {
    match content {
        Value::Array(blocks) => blocks.push(serde_json::json!({"type": "text", "text": text})),
        Value::String(existing) => *existing = format!("{existing}\n\n{text}"),
        Value::Null => *content = Value::String(text.to_string()),
        other => {
            let existing = std::mem::take(other);
            *other = Value::Array(vec![
                existing,
                serde_json::json!({"type": "text", "text": text}),
            ]);
        }
    }
}

fn prepend_text_to_content(content: &mut Value, text: &str) {
    match content {
        Value::Array(blocks) => blocks.insert(0, serde_json::json!({"type": "text", "text": text})),
        Value::String(existing) => *existing = format!("{text}\n\n{existing}"),
        Value::Null => *content = Value::String(text.to_string()),
        other => {
            let existing = std::mem::take(other);
            *other = Value::Array(vec![
                serde_json::json!({"type": "text", "text": text}),
                existing,
            ]);
        }
    }
}

fn message_content_mut(message: &mut Value) -> &mut Value {
    message
        .as_object_mut()
        .expect("conversation message is a JSON object")
        .entry("content".to_string())
        .or_insert(Value::Null)
}

fn contains_tool_result(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        })
}

/// Anthropic permits a native system section after an assistant turn whose
/// final block is a server-tool result. Future server tools follow the same
/// `*_tool_result` block naming convention, while client tool results are the
/// exact `tool_result` type on a user turn.
fn assistant_ends_with_server_tool_result(message: &Value) -> bool {
    if !is_assistant(message) {
        return false;
    }
    let Some(block_type) = message
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.last())
        .and_then(|block| block.get("type"))
        .and_then(Value::as_str)
    else {
        return false;
    };
    block_type != "tool_result" && block_type.ends_with("_tool_result")
}

/// Pull a leading run of `system`/`developer` messages off the front of the
/// conversation and merge their text into the top-level `system` prompt (the
/// existing prompt first, then the leading directives in order). Leading system
/// content is the system prompt on every route, so this is safe for all
/// placements.
fn hoist_leading_system_run(messages: &mut Vec<Value>, system: &mut Option<String>) {
    let mut leading = Vec::new();
    while messages.first().is_some_and(is_system_or_developer) {
        let message = messages.remove(0);
        let text = message_text(&message);
        if !text.trim().is_empty() {
            leading.push(text);
        }
    }
    if leading.is_empty() {
        return;
    }
    let merged = leading.join("\n\n");
    *system = Some(match system.take() {
        Some(existing) if !existing.trim().is_empty() => format!("{existing}\n\n{merged}"),
        _ => merged,
    });
}

/// Whether the consecutive directive section `[start, end)` satisfies
/// Anthropic's placement contract.
fn is_valid_native_section(messages: &[Value], start: usize, end: usize) -> bool {
    start > 0
        && (is_user(&messages[start - 1])
            || assistant_ends_with_server_tool_result(&messages[start - 1]))
        && (end == messages.len() || is_assistant(&messages[end]))
}

/// Normalize interleaved `system`/`developer` messages for a route with the
/// given [`SystemMessagePlacement`]. Mutates `messages` and `system` in place.
///
/// Consecutive directives are handled as one positional section. Native-valid
/// sections preserve every message field and content block, changing only a
/// `developer` role to Anthropic's `system` role. Folded sections attach to the
/// immediately preceding user turn, the immediately following user turn, or a
/// fresh user turn at the same boundary, in that order. This preserves
/// chronology and never splits a tool-use/result pair.
pub(crate) fn normalize_conversation(
    messages: &mut Vec<Value>,
    system: &mut Option<String>,
    placement: SystemMessagePlacement,
) {
    if matches!(placement, SystemMessagePlacement::Inline) {
        // The route carries system/developer messages verbatim at any position.
        return;
    }
    if !messages.iter().any(is_system_or_developer) {
        return;
    }

    hoist_leading_system_run(messages, system);

    let native = matches!(placement, SystemMessagePlacement::NativeDirective);
    let input = std::mem::take(messages);
    let mut out = Vec::with_capacity(input.len());
    let mut index = 0;

    while index < input.len() {
        if !is_system_or_developer(&input[index]) {
            out.push(input[index].clone());
            index += 1;
            continue;
        }

        let start = index;
        while index < input.len() && is_system_or_developer(&input[index]) {
            index += 1;
        }
        let end = index;

        if native && is_valid_native_section(&input, start, end) {
            for mut directive in input[start..end].iter().cloned() {
                directive
                    .as_object_mut()
                    .expect("directive message is a JSON object")
                    .insert("role".to_string(), Value::String("system".to_string()));
                out.push(directive);
            }
            continue;
        }

        let text = input[start..end]
            .iter()
            .map(message_text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        if text.is_empty() {
            continue;
        }
        let reminder = system_reminder_block(&text);

        if let Some(previous) = out.last_mut().filter(|message| is_user(message)) {
            append_text_to_content(message_content_mut(previous), &reminder);
        } else if index < input.len() && is_user(&input[index]) {
            let mut next = input[index].clone();
            if contains_tool_result(&next) {
                append_text_to_content(message_content_mut(&mut next), &reminder);
            } else {
                prepend_text_to_content(message_content_mut(&mut next), &reminder);
            }
            out.push(next);
            index += 1;
        } else {
            out.push(serde_json::json!({"role": "user", "content": reminder}));
        }
    }

    *messages = out;
}

/// Payload post-processor: resolve the route's placement from its capability
/// matrix and normalize the conversation in place. Called from
/// `From<&LlmCallOptions> for LlmRequestPayload`.
pub(crate) fn normalize_payload_system_messages(payload: &mut crate::llm::api::LlmRequestPayload) {
    let caps = crate::llm::capabilities::lookup(&payload.provider, &payload.model);
    let placement = resolve_system_message_placement(&caps);
    normalize_conversation(&mut payload.messages, &mut payload.system, placement);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn roles(messages: &[Value]) -> Vec<String> {
        messages
            .iter()
            .map(|m| role_of(m).unwrap_or("?").to_string())
            .collect()
    }

    #[test]
    fn inline_is_a_no_op() {
        let mut messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "system", "content": "be terse"}),
            json!({"role": "assistant", "content": "ok"}),
        ];
        let before = messages.clone();
        let mut system = None;
        normalize_conversation(&mut messages, &mut system, SystemMessagePlacement::Inline);
        assert_eq!(messages, before);
        assert_eq!(system, None);
    }

    #[test]
    fn fold_hoists_leading_system_into_prompt() {
        let mut messages = vec![
            json!({"role": "system", "content": "you are a bot"}),
            json!({"role": "user", "content": "hi"}),
        ];
        let mut system = None;
        normalize_conversation(&mut messages, &mut system, SystemMessagePlacement::Fold);
        assert_eq!(system.as_deref(), Some("you are a bot"));
        assert_eq!(roles(&messages), vec!["user"]);
    }

    #[test]
    fn fold_leading_system_appends_after_existing_prompt() {
        let mut messages = vec![
            json!({"role": "system", "content": "extra rule"}),
            json!({"role": "user", "content": "hi"}),
        ];
        let mut system = Some("base prompt".to_string());
        normalize_conversation(&mut messages, &mut system, SystemMessagePlacement::Fold);
        assert_eq!(system.as_deref(), Some("base prompt\n\nextra rule"));
    }

    #[test]
    fn fold_between_user_and_assistant_attaches_to_preceding_user() {
        let mut messages = vec![
            json!({"role": "user", "content": "U1"}),
            json!({"role": "system", "content": "reply in French"}),
            json!({"role": "assistant", "content": "A1"}),
            json!({"role": "user", "content": "U2"}),
        ];
        let mut system = None;
        normalize_conversation(&mut messages, &mut system, SystemMessagePlacement::Fold);
        assert_eq!(
            messages,
            vec![
                json!({"role": "user", "content": format!("U1\n\n{}", system_reminder_block("reply in French"))}),
                json!({"role": "assistant", "content": "A1"}),
                json!({"role": "user", "content": "U2"}),
            ]
        );
    }

    #[test]
    fn fold_after_assistant_attaches_to_following_user_without_rewriting_history() {
        let mut messages = vec![
            json!({"role": "user", "content": "U1"}),
            json!({"role": "assistant", "content": "A1"}),
            json!({"role": "system", "content": "reply in French"}),
            json!({"role": "user", "content": "U2"}),
        ];
        let mut system = None;
        normalize_conversation(&mut messages, &mut system, SystemMessagePlacement::Fold);
        assert_eq!(
            messages,
            vec![
                json!({"role": "user", "content": "U1"}),
                json!({"role": "assistant", "content": "A1"}),
                json!({"role": "user", "content": format!("{}\n\nU2", system_reminder_block("reply in French"))}),
            ]
        );
    }

    #[test]
    fn fold_trailing_after_assistant_emits_a_new_user_turn() {
        let mut messages = vec![
            json!({"role": "user", "content": "U1"}),
            json!({"role": "assistant", "content": "A1"}),
            json!({"role": "system", "content": "reply in French"}),
        ];
        let mut system = None;
        normalize_conversation(&mut messages, &mut system, SystemMessagePlacement::Fold);
        assert_eq!(
            messages,
            vec![
                json!({"role": "user", "content": "U1"}),
                json!({"role": "assistant", "content": "A1"}),
                json!({"role": "user", "content": system_reminder_block("reply in French")}),
            ]
        );
    }

    #[test]
    fn fold_between_tool_use_and_result_preserves_the_pair() {
        let mut messages = vec![
            json!({"role": "assistant", "content": [{
                "type": "tool_use", "id": "toolu_1", "name": "run", "input": {}
            }]}),
            json!({"role": "system", "content": "new constraint"}),
            json!({"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"
            }]}),
        ];
        let mut system = None;
        normalize_conversation(&mut messages, &mut system, SystemMessagePlacement::Fold);
        assert_eq!(
            messages,
            vec![
                json!({"role": "assistant", "content": [{
                    "type": "tool_use", "id": "toolu_1", "name": "run", "input": {}
                }]}),
                json!({"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"},
                    {"type": "text", "text": system_reminder_block("new constraint")},
                ]}),
            ]
        );
    }

    #[test]
    fn native_preserves_content_blocks_and_consecutive_sections() {
        let mut messages = vec![
            json!({"role": "user", "content": "U1"}),
            json!({"role": "system", "content": [{
                "type": "text",
                "text": "first",
                "cache_control": {"type": "ephemeral"}
            }]}),
            json!({"role": "developer", "content": [{"type": "text", "text": "second"}]}),
            json!({"role": "assistant", "content": "A1"}),
        ];
        let expected = vec![
            messages[0].clone(),
            messages[1].clone(),
            json!({"role": "system", "content": [{"type": "text", "text": "second"}]}),
            messages[3].clone(),
        ];
        let mut system = None;
        normalize_conversation(
            &mut messages,
            &mut system,
            SystemMessagePlacement::NativeDirective,
        );
        assert_eq!(messages, expected);
    }

    #[test]
    fn native_accepts_section_after_assistant_server_tool_result() {
        let mut messages = vec![
            json!({"role": "assistant", "content": [
                {"type": "text", "text": "searched"},
                {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": []}
            ]}),
            json!({"role": "system", "content": "fresh context"}),
            json!({"role": "assistant", "content": "A2"}),
        ];
        let expected = messages.clone();
        let mut system = None;
        normalize_conversation(
            &mut messages,
            &mut system,
            SystemMessagePlacement::NativeDirective,
        );
        assert_eq!(messages, expected);
    }

    #[test]
    fn native_invalid_section_folds_at_its_chronological_boundary() {
        let mut messages = vec![
            json!({"role": "user", "content": "U1"}),
            json!({"role": "assistant", "content": "A1"}),
            json!({"role": "system", "content": "new constraint"}),
            json!({"role": "user", "content": "U2"}),
        ];
        let mut system = None;
        normalize_conversation(
            &mut messages,
            &mut system,
            SystemMessagePlacement::NativeDirective,
        );
        assert_eq!(
            messages,
            vec![
                json!({"role": "user", "content": "U1"}),
                json!({"role": "assistant", "content": "A1"}),
                json!({"role": "user", "content": format!("{}\n\nU2", system_reminder_block("new constraint"))}),
            ]
        );
    }
}
