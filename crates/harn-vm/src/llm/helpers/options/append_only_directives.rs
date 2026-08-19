//! Append-only placement policy for the context-directive envelope.
//!
//! Every provider-side prompt cache is a prefix cache. Anthropic hashes the
//! prefix ending at each breakpoint, OpenAI matches an initial run of tokens,
//! vLLM chains one hash per KV block over the tokens preceding it, and a
//! llama.cpp slot keeps the longest common prefix and re-prefills from the
//! first divergent token to the end. The property that keeps all of them warm
//! is the same one:
//!
//!   the serialized message array at request N+1 begins with the serialized
//!   message array at request N.
//!
//! The legacy placement cannot hold that invariant. It renders the envelope
//! fresh on every request from whatever is pending at that moment, and folds
//! it into the trailing message when that message is a `user` turn. The turn
//! that carried the envelope at request N is therefore rewritten at request
//! N+1, once the conversation has grown past it — and a divergence at the
//! first user turn invalidates the whole prompt.
//!
//! Append-only placement moves the envelope into durable history at the turn
//! boundary where it fires. Later turns re-send those exact bytes at the same
//! index and append any genuinely new directive after the newer content.
//! Deduplication becomes "do not re-issue": a directive whose rendered text is
//! already committed is simply not emitted again, because removing it would
//! cost a full re-prefill of everything after it while emitting nothing costs
//! nothing. Compaction remains the one sanctioned prefix break; it starts a
//! new prefix deliberately and is already evented.

use std::collections::BTreeSet;

use super::reminders::RenderedReminder;

/// Opening tag of one rendered directive block.
const DIRECTIVE_OPEN_PREFIX: &str = "<directive authority=\"";
/// Closing tag of one rendered directive block.
const DIRECTIVE_CLOSE: &str = "</directive>";

/// Environment fallback for embedders that cannot thread `agent_loop`
/// options through to the turn builder (a measurement lane, most often).
const APPEND_ONLY_ENV: &str = "HARN_REMINDERS_APPEND_ONLY";

/// Resolve the append-only placement gate.
///
/// Off by default: the placement is model-visible, so an embedder enables it
/// and measures cache behavior before anyone proposes flipping the default.
pub(crate) fn append_only_enabled(explicit: Option<bool>) -> bool {
    if let Some(value) = explicit {
        return value;
    }
    matches!(
        crate::stdlib::process::session_env_var(APPEND_ONLY_ENV)
            .ok()
            .flatten()
            .as_deref(),
        Some("1" | "true" | "TRUE" | "yes")
    )
}

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

/// Extract every complete `<directive …>…</directive>` block from `text`, in
/// the exact serialized form [`reminder_directive_text`] produces.
///
/// [`reminder_directive_text`]: super::reminders::reminder_directive_text
fn directive_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(DIRECTIVE_OPEN_PREFIX) {
        let after_start = &rest[start..];
        let Some(end) = after_start.find(DIRECTIVE_CLOSE) else {
            break;
        };
        let end = end + DIRECTIVE_CLOSE.len();
        blocks.push(after_start[..end].to_string());
        rest = &after_start[end..];
    }
    blocks
}

/// The set of directive blocks already present in durable history.
///
/// Committed state is derived from the messages themselves rather than a side
/// table, so it survives fork, resume, replay, and compaction without a second
/// bookkeeping surface that could drift out of step with what the model saw.
fn committed_directives(messages: &[serde_json::Value]) -> BTreeSet<String> {
    messages
        .iter()
        .flat_map(|message| directive_blocks(&message_text(message)))
        .collect()
}

/// Drop the directives already committed to `messages`, preserving the order
/// of the rest. This is the "do not re-issue" half of the policy: a provider
/// that re-fires an unchanged body every turn contributes nothing new, and
/// history is never touched to remove its earlier copy.
pub(crate) fn uncommitted_directives(
    messages: &[serde_json::Value],
    rendered: &[RenderedReminder],
) -> Vec<RenderedReminder> {
    if rendered.is_empty() {
        return Vec::new();
    }
    let committed = committed_directives(messages);
    rendered
        .iter()
        .filter(|reminder| match reminder {
            RenderedReminder::SystemText(text) => !committed.contains(text),
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directive(body: &str) -> RenderedReminder {
        RenderedReminder::SystemText(format!(
            "<directive authority=\"contract\">\n{body}\n</directive>"
        ))
    }

    fn user(content: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": content})
    }

    #[test]
    fn directive_blocks_survive_an_envelope_with_several_directives() {
        let envelope = format!(
            "<context-directives>\nheader\n{}\n{}\n</context-directives>",
            directive_text(&directive("first")),
            directive_text(&directive("second")),
        );
        let blocks = directive_blocks(&envelope);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].contains("first"));
        assert!(blocks[1].contains("second"));
    }

    fn directive_text(reminder: &RenderedReminder) -> String {
        match reminder {
            RenderedReminder::SystemText(text) => text.clone(),
        }
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

    #[test]
    fn the_gate_is_off_unless_a_caller_asks_for_it() {
        assert!(!append_only_enabled(Some(false)));
        assert!(append_only_enabled(Some(true)));
    }
}
