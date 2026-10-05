//! The model's declared purpose for a tool call, as hosts receive it.
//!
//! The agent loop threads the turn's purpose label into dispatch as
//! `_purpose_label` and onto the tool-call start event as `intent`. Both the
//! approval request (`toolCall._meta.harn.intent`) and the tool-call event
//! normalize through [`normalize`], so the two surfaces cannot disagree.

/// Upper bound, in characters, on a tool call's wire `intent`.
pub(crate) const MAX_CHARS: usize = 200;

/// Normalize a declared purpose into the wire `intent`.
///
/// Whitespace runs collapse to one space so a heading cannot smuggle line
/// breaks into an approval title, and the result is bounded to [`MAX_CHARS`],
/// ending in an ellipsis when cut. Blank input yields `None`, which keeps the
/// key off the wire entirely.
pub(crate) fn normalize(raw: &str) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    if collapsed.chars().count() <= MAX_CHARS {
        return Some(collapsed);
    }
    let kept: String = collapsed.chars().take(MAX_CHARS - 1).collect();
    Some(format!("{}\u{2026}", kept.trim_end()))
}
