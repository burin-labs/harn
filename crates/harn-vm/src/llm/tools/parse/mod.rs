//! Text tool-call parsing: the reverse-direction wire format used by the
//! agent loop to read tool invocations back out of a model response.
//!
//! Exposes `parse_text_tool_calls_with_tools` + `parse_bare_calls_in_body`
//! and the `TextToolParseResult` shape; everything else is a local helper
//! (ident parser, TS literal parser, heredoc skipper, native-JSON fallback).

mod bare;
mod native_json;
mod streaming;
mod syntax;
mod tagged;

#[cfg(test)]
pub(crate) use bare::parse_bare_calls_in_body;
#[cfg(test)]
pub(crate) use native_json::parse_native_json_tool_calls;
pub(crate) use streaming::StreamingToolCallDetector;
pub(crate) use syntax::ident_length;
pub(crate) use tagged::parse_text_tool_calls_with_tools;

/// Result of parsing a prose-interleaved TS tool-call stream.
///
/// The scanner walks the model's text once and splits it into three
/// streams for the caller:
///   - `calls`: the parsed structured tool calls.
///   - `errors`: diagnostics for malformed call attempts.
///   - `prose`: the original text with every successfully-parsed call
///     expression removed, whitespace around the hole collapsed. This is
///     what should be shown as "the agent's answer" and replayed back into
///     conversation history — tool calls are structured data, not narration.
pub(crate) struct TextToolParseResult {
    pub calls: Vec<serde_json::Value>,
    pub errors: Vec<String>,
    pub prose: String,
    /// Explicit host-facing response content emitted inside one or more
    /// `<user_response>...</user_response>` blocks. When present, this is the
    /// preferred public answer surface and supersedes generic
    /// `<assistant_prose>` for `prose` rendering.
    pub user_response: Option<String>,
    /// Protocol-level grammar violations (stray text outside tags, unknown
    /// tags, unclosed tags, malformed `<done>` contents). Distinct from
    /// `errors`, which carry per-call parse diagnostics. The agent loop
    /// replays these to the model as structured `protocol_violation`
    /// feedback so it can self-correct.
    pub violations: Vec<String>,
    /// Body of the `<done>` block when one was emitted, trimmed of
    /// surrounding whitespace. The agent compares this against the
    /// pipeline's configured `done_sentinel` (default `##DONE##`) to
    /// decide whether to honor completion. Replaces substring matching
    /// against a bare sentinel string.
    pub done_marker: Option<String>,
    /// Canonical reconstruction of the response in the tagged grammar.
    /// Used as the assistant's history entry so future turns see the
    /// well-formed shape instead of the raw provider bytes.
    pub canonical: String,
}
