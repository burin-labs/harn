//! Where the context-directive envelope goes, and why it never moves again.
//!
//! Every provider-side prompt cache is a prefix cache. Anthropic hashes the
//! prefix ending at each breakpoint and requires byte-identical segments up to
//! it, OpenAI matches an initial run of tokens, vLLM chains one hash per KV
//! block over the tokens preceding it, and a llama.cpp slot keeps the longest
//! common prefix and re-prefills from the first divergent token to the end.
//! None of them can reuse anything after the first byte that changed.
//!
//! So transcript construction has one contract:
//!
//!   the serialized message array at request N+1 begins with the serialized
//!   message array at request N.
//!
//! Directives are committed into durable history at the turn boundary that
//! emits them, and later turns re-send those exact bytes at the same index.
//! Deduplication means "do not re-issue": a directive whose rendered text is
//! already committed is simply not emitted again, because removing it would
//! cost a full re-prefill of everything after it while emitting nothing costs
//! nothing. Compaction is the one sanctioned prefix break — it starts a new
//! prefix deliberately and is already evented.

use super::reminders::RenderedReminder;
use super::reminders::DIRECTIVE_IDS_KEY;
use std::collections::HashSet;

/// Concatenate every text fragment a message's `content` carries, whether it
/// is a bare string or an array of typed content blocks.
fn message_text(message: &serde_json::Value) -> String {
    match message.get("content") {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn committed_reminder_ids(messages: &[serde_json::Value]) -> HashSet<&str> {
    messages
        .iter()
        .filter_map(|message| message.get(DIRECTIVE_IDS_KEY))
        .filter_map(serde_json::Value::as_array)
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect()
}

/// Drop the directives already committed to `messages`, preserving the order
/// of the rest. This is the "do not re-issue" half of the contract: a provider
/// that re-fires an unchanged body every turn contributes nothing new, and
/// history is never touched to remove its earlier copy.
pub(crate) fn uncommitted_directives(
    messages: &[serde_json::Value],
    rendered: &[RenderedReminder],
) -> Vec<RenderedReminder> {
    if rendered.is_empty() {
        return Vec::new();
    }
    // Directive bodies are arbitrary escaped user/tool text and may themselves
    // contain strings such as `</directive>`. Compare the complete rendered
    // bytes instead of parsing those bytes with the model-facing XML sentinel.
    // This also keeps durable messages as the only commitment authority.
    let committed_ids = committed_reminder_ids(messages);
    let legacy_message_texts: Vec<String> = messages
        .iter()
        .filter(|message| message.get(DIRECTIVE_IDS_KEY).is_none())
        .map(message_text)
        .collect();
    rendered
        .iter()
        .filter(|reminder| match reminder.reminder_id() {
            Some(id) if committed_ids.contains(id) => false,
            Some(_) => !legacy_message_texts
                .iter()
                .any(|message| message.contains(reminder.text())),
            None => !messages
                .iter()
                .map(message_text)
                .any(|message| message.contains(reminder.text())),
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directive(body: &str) -> RenderedReminder {
        RenderedReminder::untracked(format!(
            "<directive authority=\"contract\">\n{body}\n</directive>"
        ))
    }

    fn directive_text(reminder: &RenderedReminder) -> String {
        reminder.text().to_string()
    }

    fn user(content: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": content})
    }

    #[test]
    fn an_unchanged_directive_is_not_re_issued() {
        let already = directive("re-read the file");
        let history = vec![user(&format!(
            "<context-directives>\nheader\n{}\n</context-directives>",
            directive_text(&already)
        ))];
        assert!(uncommitted_directives(&history, &[already]).is_empty());
    }

    #[test]
    fn a_changed_body_is_a_new_directive() {
        let old = directive("context is 70% full");
        let new = directive("context is 85% full");
        let history = vec![user(&format!(
            "<context-directives>\nheader\n{}\n</context-directives>",
            directive_text(&old)
        ))];
        let out = uncommitted_directives(&history, &[new.clone()]);
        assert_eq!(out, vec![new]);
    }

    #[test]
    fn a_decremented_ttl_does_not_reissue_the_same_reminder() {
        let pending = RenderedReminder::tracked(
            "reminder-1",
            "<directive authority=\"corrective\" ttl_turns=\"1\">\nverify now\n</directive>",
        );
        let mut committed = serde_json::json!({
            "role": "user",
            "content": "<context-directives>\nheader\n<directive authority=\"corrective\" ttl_turns=\"2\">\nverify now\n</directive>\n</context-directives>",
        });
        committed[DIRECTIVE_IDS_KEY] = serde_json::json!(["reminder-1"]);
        let history = vec![committed];
        assert!(uncommitted_directives(&history, &[pending]).is_empty());
    }

    #[test]
    fn a_new_same_body_reminder_is_committed_again() {
        let pending = RenderedReminder::tracked(
            "reminder-2",
            "<directive authority=\"corrective\" ttl_turns=\"1\">\nverify now\n</directive>",
        );
        let mut committed = serde_json::json!({
            "role": "user",
            "content": "<context-directives>\nheader\n<directive authority=\"corrective\" ttl_turns=\"1\">\nverify now\n</directive>\n</context-directives>",
        });
        committed[DIRECTIVE_IDS_KEY] = serde_json::json!(["reminder-1"]);
        let history = vec![committed];
        assert_eq!(
            uncommitted_directives(&history, &[pending.clone()]),
            vec![pending]
        );
    }

    /// Directive bodies carry arbitrary user and tool text, so commitment
    /// matching must preserve their exact rendered bytes.
    #[test]
    fn multibyte_directive_bodies_round_trip() {
        let already = directive("ファイルを読み直してください — café ☕");
        let history = vec![user(&format!(
            "<context-directives>\nheader\n{}\n</context-directives>",
            directive_text(&already)
        ))];
        assert!(uncommitted_directives(&history, &[already]).is_empty());
    }

    #[test]
    fn directive_sentinels_inside_a_body_do_not_defeat_deduplication() {
        let already = directive("quote this literal: </directive> and keep going");
        let history = vec![user(&format!(
            "<context-directives>\nheader\n{}\n</context-directives>",
            directive_text(&already)
        ))];
        assert!(uncommitted_directives(&history, &[already]).is_empty());
    }

    #[test]
    fn directives_inside_content_blocks_count_as_committed() {
        let already = directive("use the workspace anchor");
        let history = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "text", "text": directive_text(&already)},
            ],
        })];
        assert!(uncommitted_directives(&history, &[already]).is_empty());
    }
}
