//! Text tool-call parsing: the reverse-direction wire format used by the
//! agent loop to read tool invocations back out of a model response.
//!
//! Exposes `parse_text_tool_calls_with_tools` + `parse_bare_calls_in_body`
//! and the `TextToolParseResult` shape; everything else is a local helper
//! (ident parser, TS literal parser, heredoc skipper, native-JSON fallback).

mod bare;
mod fenced_json;
mod native_json;
mod streaming;
mod syntax;
mod tagged;

#[cfg(test)]
pub(crate) use bare::parse_bare_calls_in_body;
pub(crate) use fenced_json::parse_fenced_json_tool_calls;
#[cfg(test)]
pub(crate) use native_json::parse_native_json_tool_calls;
pub(crate) use streaming::StreamingToolCallDetector;
pub(crate) use syntax::ident_length;
pub(crate) use syntax::unescape_heredoc_body;
pub(crate) use syntax::{scan_heredoc, HeredocError};
pub(crate) use tagged::parse_text_tool_calls_with_tools;

/// Text-channel tool-call formats Harn understands. `tool_format == "native"`
/// is the provider JSON channel and never reaches a text parser; the two
/// values here are the text-channel grammars the agent loop can hand to
/// [`parse_text_tool_calls_in_format`].
///
/// This is the EXHAUSTIVE-MATCH GUARD seam (per the harn-bump CI gotchas): a
/// half-wired `"json"` must fail LOUDLY at the `match` below, never silently
/// fall back to the tagged/text grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextToolFormat {
    /// The canonical tagged/heredoc text grammar (`<tool_call> name({...})`).
    Tagged,
    /// The fenced-JSON grammar (```` ```tool ```` + a single `{name,args}`).
    FencedJson,
}

impl TextToolFormat {
    /// Map a `tool_format` option string to a text-channel grammar.
    ///
    /// `"text"` (and the empty/auto default) selects the tagged grammar;
    /// `"json"` selects fenced-JSON. `"native"` is the provider channel and
    /// has no text parser — callers must not route it here, so it maps to the
    /// tagged grammar only as a defensive default (the native path never calls
    /// this). Any unknown value also defaults to tagged.
    pub(crate) fn from_option(tool_format: &str) -> Self {
        match tool_format {
            "json" => TextToolFormat::FencedJson,
            // "text", "native", "auto", "", and unknown values all read text.
            _ => TextToolFormat::Tagged,
        }
    }
}

/// Parse model text into tool calls under the requested text-channel grammar.
///
/// The EXHAUSTIVE `match` here is the guard that makes a half-wired `"json"`
/// fail at compile time if a new [`TextToolFormat`] variant is added without a
/// parser, rather than silently degrading to the tagged grammar. The
/// downstream `{ id, name, arguments }` record shape is identical for both
/// grammars, so the agent loop / feedback / history are untouched.
pub(crate) fn parse_text_tool_calls_in_format(
    text: &str,
    tools_val: Option<&crate::value::VmValue>,
    format: TextToolFormat,
) -> TextToolParseResult {
    match format {
        TextToolFormat::Tagged => parse_text_tool_calls_with_tools(text, tools_val),
        TextToolFormat::FencedJson => parse_fenced_json_tool_calls(text),
    }
}

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
