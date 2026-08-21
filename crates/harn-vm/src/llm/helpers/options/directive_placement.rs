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

use std::collections::BTreeSet;

use super::reminders::RenderedReminder;

/// Opening tag of one rendered directive block.
const DIRECTIVE_OPEN_PREFIX: &str = "<directive authority=\"";
/// Closing tag of one rendered directive block.
const DIRECTIVE_CLOSE: &str = "</directive>";

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
        let Some(after_start) = rest.get(start..) else {
            break;
        };
        let Some(close) = after_start.find(DIRECTIVE_CLOSE) else {
            break;
        };
        let end = close + DIRECTIVE_CLOSE.len();
        let (Some(block), Some(remainder)) = (after_start.get(..end), after_start.get(end..))
        else {
            break;
        };
        blocks.push(block.to_string());
        rest = remainder;
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

    fn directive_text(reminder: &RenderedReminder) -> String {
        match reminder {
            RenderedReminder::SystemText(text) => text.clone(),
        }
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

    /// Directive bodies carry arbitrary user and tool text, so block scanning
    /// must not assume one byte per character.
    #[test]
    fn multibyte_directive_bodies_round_trip() {
        let already = directive("ファイルを読み直してください — café ☕");
        let history = vec![user(&format!(
            "<context-directives>\nheader\n{}\n</context-directives>",
            directive_text(&already)
        ))];
        assert_eq!(directive_blocks(&message_text(&history[0])).len(), 1);
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
