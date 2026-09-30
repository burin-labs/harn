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

#[cfg(test)]
use super::reminders::DirectiveSpeaker;
use super::reminders::RenderedReminder;
use super::reminders::DIRECTIVE_IDS_KEY;
use crate::llm::helpers::transcript::{DirectiveAuthority, ReminderSource, SystemReminder};
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

/// Fixed text restated after a withdrawn turn whose rejection carried no
/// reason of its own. Fixed, so it is not a per-run prose decision.
pub(crate) const WITHDRAWN_TURN_FALLBACK_DIRECTIVE: &str =
    "The closing report above was withdrawn because it was not accepted. \
     Continue the task before you close again.";

fn is_bookkeeping_turn(message: &serde_json::Value) -> bool {
    message
        .get(crate::llm::agent_result_projection::BOOKKEEPING_TURN_KEY)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn normalized_body(body: &str) -> String {
    body.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The directives a turn boundary commits after `messages`.
///
/// Ordinarily these are the uncommitted ones. The exception is a transcript
/// that ends on a bookkeeping turn: the placeholder a withdrawn closing draft
/// is reduced to, which tells the model to read the directive that follows.
/// Something must follow it. A request that ends on an assistant message is a
/// prefill to OpenAI-compatible servers, and two in a row are refused. And the
/// rejection is exactly what "do not re-issue" can swallow: when a standing
/// reminder already committed the same text, or the feedback composer
/// suppressed an unchanged next step, nothing is pending.
///
/// So after a bookkeeping turn the rejection directive is always among the
/// directives committed, restated when it is already in history. It is the
/// carried reminder whose body matches `withdrawal_reason` when one exists,
/// so the model reads the same bytes and authority it read before, and a
/// corrective directive carrying that reason otherwise.
pub(crate) fn turn_boundary_directives(
    messages: &[serde_json::Value],
    reminders: &[SystemReminder],
    withdrawal_reason: Option<&str>,
) -> Vec<RenderedReminder> {
    let capabilities = crate::llm::capabilities::Capabilities::default();
    let rendered = super::reminders::render_pending_reminders(&capabilities, reminders);
    let mut pending = uncommitted_directives(messages, &rendered);
    if !messages.last().is_some_and(is_bookkeeping_turn) {
        return pending;
    }
    let body = withdrawal_reason
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or(WITHDRAWN_TURN_FALLBACK_DIRECTIVE);
    let wanted = normalized_body(body);
    let carried = reminders
        .iter()
        .zip(rendered.iter())
        .find(|(reminder, _)| normalized_body(&reminder.body) == wanted)
        .map(|(_, rendered)| rendered.clone());
    let restated = carried.unwrap_or_else(|| {
        let mut reminder = SystemReminder::new(body, ReminderSource::InPipeline, 0);
        reminder.tags = vec!["withdrawn_turn".to_string()];
        reminder.authority = DirectiveAuthority::Corrective;
        super::reminders::render_pending_reminders(&capabilities, &[reminder]).remove(0)
    });
    if !pending.contains(&restated) {
        pending.push(restated);
    }
    pending
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directive(body: &str) -> RenderedReminder {
        RenderedReminder::untracked(
            format!("<directive authority=\"contract\">\n{body}\n</directive>"),
            DirectiveSpeaker::Harness,
        )
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
            DirectiveSpeaker::Harness,
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
            DirectiveSpeaker::Harness,
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

    fn withdrawn_marker() -> serde_json::Value {
        let mut marker = serde_json::json!({"role": "assistant", "content": "[withdrawn]"});
        marker[crate::llm::agent_result_projection::BOOKKEEPING_TURN_KEY] = serde_json::json!(true);
        marker
    }

    fn committed_history(reminder: &SystemReminder) -> Vec<serde_json::Value> {
        let capabilities = crate::llm::capabilities::Capabilities::default();
        let rendered =
            super::super::reminders::render_pending_reminders(&capabilities, &[reminder.clone()]);
        let envelope = super::super::reminders::directive_envelope_message(&rendered)
            .expect("one directive renders an envelope");
        vec![user("task"), envelope]
    }

    /// A standing reminder already committed the rejection text, so "do not
    /// re-issue" leaves nothing pending. After a withdrawn turn the rejection
    /// is restated anyway, carrying the committed reminder's own bytes.
    #[test]
    fn a_withdrawn_turn_restates_an_already_committed_rejection() {
        let standing = SystemReminder::new("run the tests", ReminderSource::InPipeline, 0);
        let mut history = committed_history(&standing);
        let reminders = vec![standing.clone()];
        assert!(turn_boundary_directives(&history, &reminders, Some("run the tests")).is_empty());

        history.push(withdrawn_marker());
        let out = turn_boundary_directives(&history, &reminders, Some(" run  the tests "));
        assert_eq!(out.len(), 1, "{out:#?}");
        assert_eq!(out[0].reminder_id(), Some(standing.id.as_str()));
    }

    /// No carried reminder holds the rejection (the composer suppressed it, or
    /// it expired): a corrective directive carrying the reason is committed.
    #[test]
    fn a_withdrawn_turn_with_no_carried_rejection_commits_the_reason() {
        let other = SystemReminder::new("keep it short", ReminderSource::InPipeline, 0);
        let mut history = committed_history(&other);
        history.push(withdrawn_marker());
        let out = turn_boundary_directives(&history, &[other], Some("run the tests"));
        assert_eq!(out.len(), 1, "{out:#?}");
        assert!(out[0].text().contains("run the tests"), "{out:#?}");
        assert!(out[0].text().contains("corrective"), "{out:#?}");

        let silent = turn_boundary_directives(&history, &[], None);
        assert_eq!(silent.len(), 1);
        assert!(silent[0].text().contains(WITHDRAWN_TURN_FALLBACK_DIRECTIVE));
    }

    /// A rejection that is already pending is not committed twice.
    #[test]
    fn a_pending_rejection_after_a_withdrawn_turn_is_committed_once() {
        let rejection = SystemReminder::new("run the tests", ReminderSource::InPipeline, 0);
        let history = vec![user("task"), withdrawn_marker()];
        let out = turn_boundary_directives(&history, &[rejection], Some("run the tests"));
        assert_eq!(out.len(), 1, "{out:#?}");
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
