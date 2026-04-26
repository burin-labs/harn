//! Streaming-tool-call partial-arg helpers (#693).
//!
//! Native tool calls arrive over SSE as a sequence of `input_json_delta`
//! (Anthropic) or `tool_calls[].function.arguments` deltas (OpenAI). The
//! transport accumulates the byte concatenation, then — at most every
//! `COALESCE_WINDOW` — tries to coerce the in-progress bytes into a
//! displayable shape so clients can render arguments live.
//!
//! Two shapes go on the wire:
//!   - `raw_input: Option<serde_json::Value>` when the partial bytes
//!     resolved to a JSON value (strict parse, or after the recovery
//!     pass closed dangling strings/objects/arrays).
//!   - `raw_input_partial: Option<String>` when nothing recovered — the
//!     client gets the raw concatenated bytes as a fallback so it can
//!     still surface "edit path=foo.swift, replace=…" before the parse
//!     stabilizes.

use std::time::{Duration, Instant};

/// Coalescing window for `input_json_delta` → `tool_call_update` emission.
/// Models stream tool args in tens of small chunks; without coalescing a
/// 200-char tool call could fan out to 30+ events on a slow client. The
/// 50 ms window matches what burin-code's TUI redraw cadence handles
/// without dropping frames.
pub(super) const COALESCE_WINDOW: Duration = Duration::from_millis(50);

/// Result of trying to project a streaming JSON byte buffer onto a
/// client-renderable value. Exactly one of the two `Some` variants is
/// populated for a given snapshot — both `None` is treated as "nothing
/// new to emit".
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PartialToolArgs {
    /// Best-effort parsed JSON value. Set when the strict parse or the
    /// permissive recovery pass succeeded. Mutually exclusive with
    /// `raw_partial`.
    pub value: Option<serde_json::Value>,
    /// Raw concatenated bytes when neither parse succeeded — clients
    /// render this verbatim so partial typing of e.g. unterminated
    /// string literals still shows up. Mutually exclusive with `value`.
    pub raw_partial: Option<String>,
}

impl PartialToolArgs {
    /// True when neither a parsed value nor raw bytes are present —
    /// i.e. the caller has nothing new to emit. Used by the test suite
    /// and by callers that want to short-circuit the emit path.
    #[allow(dead_code)]
    pub(super) fn is_empty(&self) -> bool {
        self.value.is_none() && self.raw_partial.is_none()
    }
}

/// Project the in-progress concatenated bytes onto the wire shape.
///
/// Strategy:
/// 1. **Strict parse first.** If the model has emitted a complete-enough
///    JSON value already (whole-number, whole-string, balanced object),
///    use that. Most short args (`{"path": "foo"}`) parse strictly the
///    moment the closing brace lands.
/// 2. **Permissive recovery** otherwise: walk the bytes, track string
///    state and bracket depth, drop the trailing partial token, and
///    close any dangling `"`/`{`/`[`. Re-strict-parse the result.
/// 3. **Raw fallback** if recovery still fails — return the raw bytes
///    so the client at least surfaces the partial typing.
///
/// The recovery deliberately never invents missing values — a dangling
/// `"path":` reduces to the previous valid prefix `{"path": null}` only
/// if the caller can synthesize it cheaply. Right now we conservatively
/// fall through to the raw-bytes path, which keeps the parser tiny and
/// matches the issue's "side-band field carrying the raw concatenated
/// bytes" contract verbatim.
pub(super) fn project_partial(bytes: &str) -> PartialToolArgs {
    let trimmed = bytes.trim_start();
    if trimmed.is_empty() {
        return PartialToolArgs {
            value: None,
            raw_partial: None,
        };
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return PartialToolArgs {
            value: Some(value),
            raw_partial: None,
        };
    }
    if let Some(closed) = close_dangling_json(trimmed) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&closed) {
            return PartialToolArgs {
                value: Some(value),
                raw_partial: None,
            };
        }
    }
    PartialToolArgs {
        value: None,
        raw_partial: Some(bytes.to_string()),
    }
}

/// Drop any trailing partial token and close dangling string / object /
/// array delimiters so a strict JSON parse has a chance.
///
/// Walks the buffer as a small state machine tracking the bracket stack
/// and whether we're inside a string / number / identifier token. At
/// every "clean point" — a position immediately after a complete value
/// or structural delimiter — we snapshot the bracket stack. After the
/// walk we truncate to the latest clean point and close the brackets
/// from that snapshot, which yields a strict-parseable JSON value when
/// the prefix is recoverable. Returns `None` for inputs that are
/// outright malformed (mismatched closers, bare colon, etc.) so the
/// caller falls through to the `raw_input_partial` path.
fn close_dangling_json(text: &str) -> Option<String> {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum State {
        Outside,
        InString,
        InNumber,
        InIdent,
    }
    let mut state = State::Outside;
    let mut escape = false;
    let mut stack: Vec<char> = Vec::new();
    // Snapshot of `stack` at the last clean cut. Captured so the close
    // pass knows exactly which brackets were open at the cut.
    let mut clean_stack: Vec<char> = Vec::new();
    let mut clean_end: usize = 0;
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        let ch = bytes[i] as char;
        match state {
            State::InString => {
                if escape {
                    escape = false;
                } else if ch == '\\' {
                    escape = true;
                } else if ch == '"' {
                    state = State::Outside;
                    clean_end = i + 1;
                    clean_stack = stack.clone();
                }
                i += 1;
            }
            State::InNumber => {
                if ch.is_ascii_digit()
                    || ch == '.'
                    || ch == 'e'
                    || ch == 'E'
                    || ch == '+'
                    || ch == '-'
                {
                    i += 1;
                } else {
                    // Number ends at i. Mark the prior position as a
                    // clean cut and re-process this char in Outside
                    // state.
                    state = State::Outside;
                    clean_end = i;
                    clean_stack = stack.clone();
                }
            }
            State::InIdent => {
                if ch.is_ascii_alphabetic() {
                    i += 1;
                } else {
                    state = State::Outside;
                    // Only count the identifier as a complete token if
                    // it's one of the JSON literals.
                    let start = clean_end;
                    let token = &text[start..i].trim_start();
                    if matches!(*token, "true" | "false" | "null") {
                        clean_end = i;
                        clean_stack = stack.clone();
                    }
                    // else: leave clean_end alone; the half-typed
                    // identifier will be dropped.
                }
            }
            State::Outside => {
                match ch {
                    '"' => {
                        state = State::InString;
                        i += 1;
                    }
                    '{' | '[' => {
                        stack.push(ch);
                        i += 1;
                        clean_end = i;
                        clean_stack = stack.clone();
                    }
                    '}' => {
                        if stack.pop() != Some('{') {
                            return None;
                        }
                        i += 1;
                        clean_end = i;
                        clean_stack = stack.clone();
                    }
                    ']' => {
                        if stack.pop() != Some('[') {
                            return None;
                        }
                        i += 1;
                        clean_end = i;
                        clean_stack = stack.clone();
                    }
                    ',' | ':' => {
                        // Structural separators always demand a value
                        // afterwards — don't treat them as a clean cut.
                        // Recovery will roll back to the prior cut.
                        i += 1;
                    }
                    ' ' | '\t' | '\n' | '\r' => {
                        i += 1;
                    }
                    '-' => {
                        state = State::InNumber;
                        i += 1;
                    }
                    c if c.is_ascii_digit() => {
                        state = State::InNumber;
                        i += 1;
                    }
                    c if c.is_ascii_alphabetic() => {
                        state = State::InIdent;
                        i += 1;
                    }
                    _ => return None,
                }
            }
        }
    }
    // If we ended mid-token, decide whether to count it as a clean
    // cut. Numbers that spanned to EOL are a complete primitive in JSON
    // grammar, so promote them. Identifiers are only complete if
    // they're a literal.
    if state == State::InNumber {
        clean_end = len;
        clean_stack = stack.clone();
    } else if state == State::InIdent {
        let token = text[clean_end..].trim_start();
        if matches!(token, "true" | "false" | "null") {
            clean_end = len;
            clean_stack = stack.clone();
        }
    }
    if clean_end == 0 {
        return None;
    }
    let mut closed = text[..clean_end].to_string();
    while let Some(open) = clean_stack.pop() {
        closed.push(if open == '{' { '}' } else { ']' });
    }
    Some(closed)
}

/// Per-tool-block coalescing gate. Records the last emit time so the
/// transport can throttle delta-driven `ToolCallUpdate` events to one
/// per [`COALESCE_WINDOW`]. The first call (`last_emit_at` still equal
/// to the construction `Instant`) returns true so clients see the very
/// first delta with zero latency.
pub(super) struct DeltaCoalescer {
    last_emit_at: Option<Instant>,
}

impl DeltaCoalescer {
    pub(super) fn new() -> Self {
        Self { last_emit_at: None }
    }

    /// Record that an emission happened at `now` (or `Instant::now()` if
    /// `now` is `None`). Returns whether the caller should emit.
    pub(super) fn should_emit(&mut self, now: Instant) -> bool {
        match self.last_emit_at {
            None => {
                self.last_emit_at = Some(now);
                true
            }
            Some(last) if now.saturating_duration_since(last) >= COALESCE_WINDOW => {
                self.last_emit_at = Some(now);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_parse_returns_value() {
        let parsed = project_partial(r#"{"path":"README.md"}"#);
        assert_eq!(parsed.value, Some(serde_json::json!({"path": "README.md"})));
        assert!(parsed.raw_partial.is_none());
    }

    #[test]
    fn unterminated_string_falls_back_to_raw() {
        // Recovery cannot synthesize the missing value, so the raw
        // bytes go on the wire so the client can still surface
        // "edit path=fo…".
        let parsed = project_partial(r#"{"path":"fo"#);
        assert!(parsed.value.is_none());
        assert_eq!(parsed.raw_partial.as_deref(), Some(r#"{"path":"fo"#));
    }

    #[test]
    fn unbalanced_object_recovers_to_value() {
        let parsed = project_partial(r#"{"path":"README.md""#);
        // String + dangling closing brace are recoverable.
        assert_eq!(parsed.value, Some(serde_json::json!({"path": "README.md"})));
    }

    #[test]
    fn unbalanced_array_recovers_to_value() {
        let parsed = project_partial(r#"{"items":[1, 2"#);
        assert_eq!(parsed.value, Some(serde_json::json!({"items": [1, 2]})));
    }

    #[test]
    fn empty_input_emits_nothing() {
        let parsed = project_partial("");
        assert!(parsed.is_empty());
    }

    #[test]
    fn whitespace_only_emits_nothing() {
        let parsed = project_partial("   \n\t  ");
        assert!(parsed.is_empty());
    }

    #[test]
    fn coalescer_emits_first_then_throttles() {
        let mut c = DeltaCoalescer::new();
        let t0 = Instant::now();
        assert!(c.should_emit(t0), "first delta must always emit");
        // 10ms later — well under the window.
        assert!(
            !c.should_emit(t0 + Duration::from_millis(10)),
            "deltas inside the coalesce window must be dropped"
        );
        assert!(
            !c.should_emit(t0 + Duration::from_millis(40)),
            "deltas inside the coalesce window must be dropped"
        );
        assert!(
            c.should_emit(t0 + Duration::from_millis(60)),
            "deltas past the coalesce window must emit"
        );
    }

    #[test]
    fn nested_object_in_array_recovers() {
        let parsed = project_partial(r#"{"a": [{"k": "v""#);
        assert_eq!(parsed.value, Some(serde_json::json!({"a": [{"k": "v"}]})));
    }

    #[test]
    fn dangling_colon_falls_back_to_raw() {
        let parsed = project_partial(r#"{"path":"#);
        // No clean cut after the colon, can't synthesize a value.
        assert!(parsed.value.is_none());
        assert_eq!(parsed.raw_partial.as_deref(), Some(r#"{"path":"#));
    }
}
