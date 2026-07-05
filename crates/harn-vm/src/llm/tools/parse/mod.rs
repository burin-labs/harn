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
pub(crate) use syntax::unwrap_fully_wrapping_heredoc;
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

/// Parse the argument payload from a provider-native tool call that appears to
/// contain Harn's text-tool syntax rather than strict JSON.
///
/// Some OpenAI-compatible providers receive a text-tool prompt but still
/// surface the model's action through `tool_calls[].function.arguments`. In
/// that case the argument string may be `{ path: "a.rs", content: <<EOF ... }`
/// or even `edit({ ... })`. Recover those complete text-format payloads
/// without requiring a registered tool schema; callers still own normalizing
/// the final `(name, arguments)` pair.
pub(crate) fn parse_text_tool_argument_payload(
    text: &str,
    name: &str,
) -> Result<serde_json::Value, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::json!({}));
    }

    match syntax::parse_object_literal_from(trimmed, name) {
        Ok((arguments, consumed)) if trimmed[consumed..].trim().is_empty() => Ok(arguments),
        Ok((_arguments, consumed)) => Err(format!(
            "trailing bytes after object literal argument at byte {consumed}"
        )),
        Err(object_error) => {
            if let Some(name_len) = syntax::ident_length(trimmed.as_bytes()) {
                if trimmed.as_bytes().get(name_len) == Some(&b'(') {
                    let call_name = trimmed[..name_len].to_string();
                    match syntax::parse_ts_call_from(trimmed, call_name) {
                        Ok((arguments, consumed)) if trimmed[consumed..].trim().is_empty() => {
                            return Ok(arguments);
                        }
                        Ok((_arguments, consumed)) => {
                            return Err(format!(
                                "trailing bytes after tool-call expression at byte {consumed}"
                            ));
                        }
                        Err(call_error) => {
                            return Err(format!("{object_error}; {call_error}"));
                        }
                    }
                }
            }
            Err(object_error)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NativeToolNameTextCall {
    NotCall,
    Parsed {
        name: String,
        arguments: serde_json::Value,
    },
    Malformed {
        name: String,
        error: String,
    },
}

/// Recover Harn text-tool syntax that an OpenAI-compatible provider misplaced
/// into `tool_calls[].function.arguments` under a generic wrapper name such as
/// `tool_call`.
///
/// Some providers surface a wrapper function named `tool_call` while placing
/// the real text-format call in the arguments string, e.g.
/// `<tool_call>\nlook({ file: "a.rs" })\n</tool_call>`. This is the same
/// recovery as [`parse_text_tool_call_from_native_name`], with text-tool wrapper
/// tags stripped before parsing the inner call expression.
pub(crate) fn parse_text_tool_call_from_native_arguments(text: &str) -> NativeToolNameTextCall {
    let unwrapped = syntax::strip_tool_call_wrappers(text);
    parse_text_tool_call_from_native_name(unwrapped.as_ref())
}

/// Recover Harn text-tool syntax that an OpenAI-compatible provider misplaced
/// into `tool_calls[].function.name`.
///
/// Z.ai/GLM has been observed returning a native-looking tool envelope while
/// copying the complete text-tool call into the `name` slot and leaving
/// `arguments` empty, e.g. `look({ file: "a.rs" })</arg_value>`. Treat complete
/// calls as the structured call they meant, and fail closed on partial calls so
/// the runtime does not dispatch a bogus tool named `edit({ ...`.
pub(crate) fn parse_text_tool_call_from_native_name(text: &str) -> NativeToolNameTextCall {
    let trimmed = strip_native_name_provider_suffixes(text);
    let Some(name_len) = syntax::ident_length(trimmed.as_bytes()) else {
        return NativeToolNameTextCall::NotCall;
    };
    if trimmed.as_bytes().get(name_len) != Some(&b'(') {
        return NativeToolNameTextCall::NotCall;
    }

    let name = trimmed[..name_len].to_string();
    match syntax::parse_ts_call_from(trimmed, name.clone()) {
        Ok((arguments, consumed)) => {
            let trailing = trimmed[consumed..].trim();
            if trailing.is_empty() {
                NativeToolNameTextCall::Parsed { name, arguments }
            } else {
                NativeToolNameTextCall::Malformed {
                    name,
                    error: format!("trailing bytes after tool-call expression at byte {consumed}"),
                }
            }
        }
        Err(error) => NativeToolNameTextCall::Malformed { name, error },
    }
}

fn strip_native_name_provider_suffixes(mut text: &str) -> &str {
    loop {
        let trimmed = text.trim();
        let Some(next) = trimmed
            .strip_suffix("</arg_value>")
            .or_else(|| trimmed.strip_suffix("</tool_call>"))
            .or_else(|| trimmed.strip_suffix("</toolcall>"))
        else {
            return trimmed;
        };
        text = next;
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

#[cfg(test)]
mod tests {
    use super::{
        parse_text_tool_argument_payload, parse_text_tool_call_from_native_name,
        NativeToolNameTextCall,
    };

    #[test]
    fn text_tool_argument_payload_parses_object_literal_heredoc() {
        let parsed = parse_text_tool_argument_payload(
            r#"{ action: "create", path: "src/main.rs", content: <<EOF
fn main() {
    println!("hello");
}
EOF
}"#,
            "edit",
        )
        .expect("object literal payload parses");

        assert_eq!(parsed["action"], serde_json::json!("create"));
        assert_eq!(parsed["path"], serde_json::json!("src/main.rs"));
        assert!(
            parsed["content"]
                .as_str()
                .is_some_and(|content| content.contains("println!(\"hello\")")),
            "content should come from the heredoc body: {parsed:?}"
        );
    }

    #[test]
    fn text_tool_argument_payload_parses_wrapped_call() {
        let parsed = parse_text_tool_argument_payload(
            r#"edit({ action: "replace_range", path: "src/lib.rs", range_start: 1, range_end: 2 })"#,
            "edit",
        )
        .expect("wrapped call payload parses");

        assert_eq!(parsed["action"], serde_json::json!("replace_range"));
        assert_eq!(parsed["path"], serde_json::json!("src/lib.rs"));
        assert_eq!(parsed["range_start"], serde_json::json!(1));
    }

    #[test]
    fn text_tool_argument_payload_rejects_trailing_bytes() {
        let error = parse_text_tool_argument_payload(
            r#"{ action: "create", path: "src/main.rs" } trailing"#,
            "edit",
        )
        .expect_err("trailing bytes should fail");

        assert!(
            error.contains("trailing bytes"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn native_name_text_call_recovers_complete_call_with_provider_suffix() {
        let parsed = parse_text_tool_call_from_native_name(
            r#"look({ file: "include/kvdb/status.h", intent: "read" })</arg_value>"#,
        );

        match parsed {
            NativeToolNameTextCall::Parsed { name, arguments } => {
                assert_eq!(name, "look");
                assert_eq!(
                    arguments["file"],
                    serde_json::json!("include/kvdb/status.h")
                );
                assert_eq!(arguments["intent"], serde_json::json!("read"));
            }
            other => panic!("expected recovered text tool call, got {other:?}"),
        }
    }

    #[test]
    fn native_name_text_call_rejects_partial_heredoc_call() {
        let parsed = parse_text_tool_call_from_native_name(
            r#"edit({ action: "create", path: "tests/page_cache.cpp", content: <<EOF"#,
        );

        match parsed {
            NativeToolNameTextCall::Malformed { name, error } => {
                assert_eq!(name, "edit");
                assert!(
                    error.contains("unexpected end of input")
                        || error.contains("expected newline after heredoc tag")
                        || error.contains("missing closing `)`"),
                    "unexpected error: {error}"
                );
            }
            other => panic!("expected malformed text tool call, got {other:?}"),
        }
    }

    #[test]
    fn native_name_text_call_ignores_plain_native_name() {
        assert_eq!(
            parse_text_tool_call_from_native_name("look"),
            NativeToolNameTextCall::NotCall
        );
    }
}
