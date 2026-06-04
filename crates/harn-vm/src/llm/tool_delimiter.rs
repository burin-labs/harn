//! Wire-level tool-call delimiter remapping for Hermes-family models.
//!
//! Some models reserve `<tool_call>` / `</tool_call>` as **single special
//! tokens** in their tokenizer (the native Hermes tool-call markers used by
//! Qwen and several derivative finetunes). Harn's text tool format reuses those
//! exact strings as block delimiters, embedding them as instructional and
//! wrapper text throughout the system prompt. When the model meets its own
//! reserved tool-call token in dozens of out-of-distribution positions it
//! collapses into degenerate opener repetition (`<tool_call>\n<tool_call>\n…`),
//! with literal-char `_call>` fragments leaking against the special token.
//!
//! For models flagged `reserved_tool_call_token` in `capabilities.toml`, we
//! swap the two colliding delimiters for a non-special bracket form on the wire
//! to the model, and swap them back before the canonical parser and transcript
//! ever see the completion. Everything upstream and downstream stays canonical
//! `<tool_call>`; only the bytes on the wire change. The bracket form was
//! validated empirically against Qwen3.6 (`[[CALL]]`: 5/5 no-collapse vs
//! `<tool_call>`: 0/5).
//!
//! Only `<tool_call>`/`</tool_call>` are remapped — the sibling protocol tags
//! (`<assistant_prose>`, `<user_response>`, `<done>`) are ordinary multi-token
//! text in these tokenizers and do not trigger the collapse.
//!
//! ## Mid-transcript model switch
//!
//! The remap is a pure wire transform: the transcript and every cache key stay
//! canonical (`request_hash` keys on `model` + canonical messages), and the
//! swap is re-derived from the full canonical history on every turn. So a
//! conversation that switches between a reserved-token model and a normal one
//! is always *correct* — there is never a half-remapped prefix, and the
//! model-keyed result cache cannot serve one regime's output for the other. The
//! only cost of a regime flip is a one-time prefix-cache miss on the new
//! model's server (the re-delimited prefix differs), which is unavoidable and
//! self-healing; forcing a compaction at that boundary would be a pure latency
//! optimization, not a correctness requirement.

use crate::llm::tools::{TEXT_TOOL_CALL_CLOSE, TEXT_TOOL_CALL_OPEN};

/// Non-special wire delimiter substituted for `<tool_call>`.
pub(crate) const WIRE_TOOL_CALL_OPEN: &str = "[[CALL]]";
/// Non-special wire delimiter substituted for `</tool_call>`.
pub(crate) const WIRE_TOOL_CALL_CLOSE: &str = "[[/CALL]]";

/// Rewrite outgoing prompt text from canonical `<tool_call>` delimiters to the
/// non-special wire form. Applied to every message (system + history) sent to a
/// `reserved_tool_call_token` model.
pub(crate) fn canonical_to_wire(text: &str) -> String {
    if !text.contains(TEXT_TOOL_CALL_OPEN) && !text.contains(TEXT_TOOL_CALL_CLOSE) {
        return text.to_string();
    }
    text.replace(TEXT_TOOL_CALL_OPEN, WIRE_TOOL_CALL_OPEN)
        .replace(TEXT_TOOL_CALL_CLOSE, WIRE_TOOL_CALL_CLOSE)
}

/// Rewrite an incoming completion from the non-special wire form back to
/// canonical `<tool_call>` delimiters before the parser/transcript see it.
pub(crate) fn wire_to_canonical(text: &str) -> String {
    if !text.contains(WIRE_TOOL_CALL_OPEN) && !text.contains(WIRE_TOOL_CALL_CLOSE) {
        return text.to_string();
    }
    // Close before open: neither is a substring of the other, so order is not
    // load-bearing, but this keeps the intent explicit.
    text.replace(WIRE_TOOL_CALL_CLOSE, TEXT_TOOL_CALL_CLOSE)
        .replace(WIRE_TOOL_CALL_OPEN, TEXT_TOOL_CALL_OPEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_canonical_delimiters() {
        let canonical = "<tool_call>\nlook({ file: \"src\" })\n</tool_call>";
        let wire = canonical_to_wire(canonical);
        assert_eq!(wire, "[[CALL]]\nlook({ file: \"src\" })\n[[/CALL]]");
        assert_eq!(wire_to_canonical(&wire), canonical);
    }

    #[test]
    fn leaves_text_without_delimiters_untouched() {
        let plain = "no tool calls here, just prose and code: let x = [[a]];";
        assert_eq!(canonical_to_wire(plain), plain);
        assert_eq!(wire_to_canonical(plain), plain);
    }

    #[test]
    fn rewrites_every_occurrence() {
        let canonical = "<tool_call>a</tool_call> then <tool_call>b</tool_call>";
        let wire = canonical_to_wire(canonical);
        assert_eq!(wire.matches(WIRE_TOOL_CALL_OPEN).count(), 2);
        assert_eq!(wire.matches(WIRE_TOOL_CALL_CLOSE).count(), 2);
        assert_eq!(wire_to_canonical(&wire), canonical);
    }
}
