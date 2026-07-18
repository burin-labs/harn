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
//! - **Anthropic Opus 4.8** accepts an interleaved `system` directive, but only
//!   when it follows a `user` turn (or an assistant turn ending in a server-tool
//!   result), is the last message or is followed by an `assistant` turn, is not
//!   `messages[0]`, and is text-only. Anywhere else — or on any older/other
//!   Claude — the Messages API rejects it with HTTP 400.
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

/// Flatten a message's `content` to its plain text. Handles the two shapes a
/// directive ever uses — a bare string, or an array of `{type:"text", text}`
/// blocks — and best-effort-joins any text found in a richer array.
fn message_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Whether a message's `content` is purely text (a string, or an array whose
/// every block is a `text` block). Anthropic's native mid-conversation system
/// directive must be text-only; a directive carrying images or tool results
/// folds instead.
fn is_text_only(message: &Value) -> bool {
    match message.get("content") {
        Some(Value::String(_)) => true,
        Some(Value::Array(blocks)) => blocks
            .iter()
            .all(|block| block.get("type").and_then(Value::as_str).unwrap_or("text") == "text"),
        None => true,
        _ => false,
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

/// Fold `text` into the trailing message when it is a `user` turn; otherwise
/// return `false` so the caller appends a fresh trailing `user` message.
/// Appending strictly after the final message never splits a tool_use /
/// tool_result pair.
fn try_append_to_last_user(messages: &mut [Value], text: &str) -> bool {
    let Some(last) = messages.last_mut() else {
        return false;
    };
    if !is_user(last) {
        return false;
    }
    let content = last
        .as_object_mut()
        .expect("user message is a JSON object")
        .entry("content".to_string())
        .or_insert(Value::Null);
    append_text_to_content(content, text);
    true
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

/// Whether the directive at `index` satisfies Anthropic's strict
/// mid-conversation placement rules (a conservative subset that never 400s):
/// text-only, non-empty, not `messages[0]`, immediately preceded by a `user`
/// turn, and either last or immediately followed by an `assistant` turn.
fn is_valid_native_directive(messages: &[Value], index: usize) -> bool {
    let message = &messages[index];
    is_text_only(message)
        && !message_text(message).trim().is_empty()
        && index > 0
        && is_user(&messages[index - 1])
        && (index + 1 == messages.len() || is_assistant(&messages[index + 1]))
}

/// Normalize interleaved `system`/`developer` messages for a route with the
/// given [`SystemMessagePlacement`]. Mutates `messages` and `system` in place.
///
/// `keep_native` decides, per directive index (into the post-hoist array),
/// whether a directive is emitted verbatim as a native `role: "system"` message
/// or folded into the conversation as a `<system-reminder>` user block. `Fold`
/// keeps nothing native; `NativeDirective` keeps validly-placed directives.
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
    let keep: Vec<bool> = (0..messages.len())
        .map(|index| {
            native
                && is_system_or_developer(&messages[index])
                && is_valid_native_directive(messages, index)
        })
        .collect();

    // Split into "kept" slots (real turns + native-kept directives, carrying
    // their original index) and "folds" (directive index → reminder text).
    let mut slots: Vec<(usize, Value)> = Vec::new();
    let mut folds: Vec<(usize, String)> = Vec::new();
    for (index, message) in std::mem::take(messages).into_iter().enumerate() {
        if is_system_or_developer(&message) {
            if keep[index] {
                // Canonicalize to a text-only `system` directive — Anthropic
                // has no `developer` role and rejects non-text directive content.
                slots.push((
                    index,
                    serde_json::json!({"role": "system", "content": message_text(&message)}),
                ));
            } else {
                let text = message_text(&message);
                if !text.trim().is_empty() {
                    folds.push((index, system_reminder_block(&text)));
                }
            }
        } else {
            slots.push((index, message));
        }
    }

    // Attach each folded directive to the nearest user turn at or after its
    // position ("from now on"); failing that, the nearest user turn before it;
    // failing that, a trailing user turn. Append-only, so a user turn carrying
    // tool_result blocks is never split.
    let mut appends: std::collections::BTreeMap<usize, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut trailing: Vec<String> = Vec::new();
    for (directive_index, text) in folds {
        let target = slots
            .iter()
            .position(|(slot_index, message)| *slot_index > directive_index && is_user(message))
            .or_else(|| {
                slots.iter().rposition(|(slot_index, message)| {
                    *slot_index < directive_index && is_user(message)
                })
            });
        match target {
            Some(slot) => appends.entry(slot).or_default().push(text),
            None => trailing.push(text),
        }
    }

    let mut out: Vec<Value> = Vec::with_capacity(slots.len() + 1);
    for (slot, (_, mut message)) in slots.into_iter().enumerate() {
        if let Some(texts) = appends.get(&slot) {
            let joined = texts.join("\n\n");
            let content = message
                .as_object_mut()
                .expect("conversation message is a JSON object")
                .entry("content".to_string())
                .or_insert(Value::Null);
            append_text_to_content(content, &joined);
        }
        out.push(message);
    }
    if !trailing.is_empty() {
        let joined = trailing.join("\n\n");
        if !try_append_to_last_user(&mut out, &joined) {
            out.push(serde_json::json!({"role": "user", "content": joined}));
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
    fn fold_midconv_system_attaches_to_following_user() {
        let mut messages = vec![
            json!({"role": "user", "content": "my name is Ada"}),
            json!({"role": "assistant", "content": "hi Ada"}),
            json!({"role": "system", "content": "reply in French"}),
            json!({"role": "user", "content": "what is my name?"}),
        ];
        let mut system = None;
        normalize_conversation(&mut messages, &mut system, SystemMessagePlacement::Fold);
        assert_eq!(roles(&messages), vec!["user", "assistant", "user"]);
        // The directive folds forward onto the following user turn.
        let last = messages.last().unwrap();
        let content = last.get("content").unwrap().as_str().unwrap();
        assert!(content.contains("<system-reminder>"), "got: {content}");
        assert!(content.contains("reply in French"));
        assert!(content.contains("what is my name?"));
    }

    #[test]
    fn fold_trailing_system_attaches_to_preceding_user_and_never_400s() {
        // A directive with no following user turn folds backward onto the last
        // user turn rather than emitting an unsupported role.
        let mut messages = vec![
            json!({"role": "user", "content": "translate this"}),
            json!({"role": "system", "content": "reply in French"}),
        ];
        let mut system = None;
        normalize_conversation(&mut messages, &mut system, SystemMessagePlacement::Fold);
        assert_eq!(roles(&messages), vec!["user"]);
        let content = messages[0].get("content").unwrap().as_str().unwrap();
        assert!(content.contains("translate this"));
        assert!(content.contains("<system-reminder>"));
    }

    #[test]
    fn fold_developer_role_is_folded_like_system() {
        let mut messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "developer", "content": "be terse"}),
        ];
        let mut system = None;
        normalize_conversation(&mut messages, &mut system, SystemMessagePlacement::Fold);
        assert_eq!(roles(&messages), vec!["user"]);
        assert!(messages[0]
            .get("content")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("be terse"));
    }

    #[test]
    fn native_keeps_validly_placed_directive() {
        // [user, system] — system follows a user turn and is last: valid native.
        let mut messages = vec![
            json!({"role": "user", "content": "my name is Ada. what language?"}),
            json!({"role": "system", "content": "reply in French"}),
        ];
        let mut system = None;
        normalize_conversation(
            &mut messages,
            &mut system,
            SystemMessagePlacement::NativeDirective,
        );
        assert_eq!(roles(&messages), vec!["user", "system"]);
        // Canonicalized to text content.
        assert_eq!(
            messages[1].get("content").unwrap().as_str(),
            Some("reply in French")
        );
    }

    #[test]
    fn native_folds_invalid_placement_instead_of_400ing() {
        // [user, assistant, system, user] — system follows an assistant turn:
        // invalid native placement, so it folds rather than reaching the wire.
        let mut messages = vec![
            json!({"role": "user", "content": "my name is Ada"}),
            json!({"role": "assistant", "content": "hi"}),
            json!({"role": "system", "content": "reply in French"}),
            json!({"role": "user", "content": "what is my name?"}),
        ];
        let mut system = None;
        normalize_conversation(
            &mut messages,
            &mut system,
            SystemMessagePlacement::NativeDirective,
        );
        assert_eq!(roles(&messages), vec!["user", "assistant", "user"]);
        assert!(messages
            .last()
            .unwrap()
            .get("content")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("reply in French"));
    }

    #[test]
    fn native_maps_developer_directive_to_system() {
        let mut messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "developer", "content": "be terse"}),
        ];
        let mut system = None;
        normalize_conversation(
            &mut messages,
            &mut system,
            SystemMessagePlacement::NativeDirective,
        );
        assert_eq!(roles(&messages), vec!["user", "system"]);
    }

    #[test]
    fn native_folds_non_text_directive() {
        // A directive carrying a non-text block cannot ride Anthropic's native
        // text-only channel; it folds instead.
        let mut messages = vec![
            json!({"role": "user", "content": "hi"}),
            json!({"role": "system", "content": [{"type": "image", "source": {}}]}),
            json!({"role": "assistant", "content": "ok"}),
        ];
        let mut system = None;
        normalize_conversation(
            &mut messages,
            &mut system,
            SystemMessagePlacement::NativeDirective,
        );
        // No system role survives; the (empty-text) directive is dropped.
        assert!(!messages.iter().any(is_system_or_developer));
    }
}
