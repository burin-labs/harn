//! Response-ingestion losses, on the event bus (harn#5142).
//!
//! Ingestion is an allowlist of block, item, and part types, and its
//! fall-through arms used to discard whatever the allowlist did not name. The
//! only downstream signal was `empty_generation_error`, which fires only when
//! *every* block was dropped — so a response with one recognized text block and
//! ten dropped ones passed as a complete answer, and a provider adding a
//! content type silently subtracted from every turn until someone noticed the
//! model "not doing what it said".
//!
//! One reporting function per drop shape, so each fall-through arm in the
//! provider response adapters stays a single readable line and the wording of
//! these events has one owner.

use serde_json::Value;

use crate::boundary::{BoundaryFailure, BoundaryFailureKind, BoundaryId};

fn report(detail: String, element: &Value) {
    BoundaryFailure::new(
        BoundaryId::ResponseContentExtraction,
        BoundaryFailureKind::Unrecognized,
        detail,
    )
    .with_excerpt(&element.to_string())
    .report();
}

/// Render an optional block/part type for a human triaging a dead turn.
fn named(kind: Option<&str>) -> &str {
    kind.unwrap_or("<none>")
}

/// An Anthropic-style `content[]` block the runtime has no handler for —
/// `redacted_thinking`, a hosted-tool result, or a type the provider added
/// after this code was written.
pub(super) fn unhandled_content_block(provider: &str, kind: Option<&str>, block: &Value) {
    report(
        format!(
            "{provider} response content block type `{}` has no handler",
            named(kind)
        ),
        block,
    );
}

/// An OpenAI Responses `output[]` item with no handler.
pub(super) fn unhandled_output_item(kind: &str, item: &Value) {
    report(
        format!("openai-responses output item type `{kind}` has no handler"),
        item,
    );
}

/// A message content part with no handler.
pub(super) fn unhandled_message_part(kind: Option<&str>, content: &Value) {
    report(
        format!(
            "openai-responses message part type `{}` has no handler",
            named(kind)
        ),
        content,
    );
}

/// A message content part whose payload is not a string — audio, an image, or
/// a structured refusal object. It is recognized but unreadable, which loses
/// just as much as an unrecognized type.
pub(super) fn unreadable_message_part(kind: Option<&str>, content: &Value) {
    report(
        format!(
            "openai-responses message part of type `{}` carries no string text",
            named(kind)
        ),
        content,
    );
}

/// Completions past `choices[0]`. `n > 1` is not a shape the runtime supports,
/// and silently keeping one of several made an unsupported request look like a
/// served one.
///
/// Takes the whole `choices` array and no-ops on the single-choice case, so the
/// caller stays one unconditional line and the "is there anything to report"
/// rule has one owner.
pub(super) fn discarded_choices(provider: &str, choices: &[Value]) {
    if choices.len() < 2 {
        return;
    }
    report(
        format!(
            "{provider} returned {} choices; only choices[0] is read",
            choices.len()
        ),
        &Value::Array(choices[1..].to_vec()),
    );
}

#[cfg(test)]
mod tests;
