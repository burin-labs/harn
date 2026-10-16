//! Registry-owned diagnostics added at the public tool-parse boundary.

use crate::llm::tools;
use crate::value::{VmError, VmValue};

/// Cause-named feedback for a tool call whose arguments could not be parsed and
/// arrived as a `{"__parse_error": "..."}` carrier. Splits on the parser
/// diagnostic:
///
/// - a TRUNCATION (`EOF while parsing` / `unexpected end of input`): the
///   streamed arguments were cut off mid-value — the model authored a valid
///   call, but the response ended before the arguments finished. Coach a
///   smaller re-issue, exactly like the length-truncation empty-args case; the
///   parser diagnostic is the authoritative signal here because the provider
///   often does NOT flag this with `finish_reason=length` (observed on
///   llamacpp: the stream stops mid-tool-call with a clean stop reason).
/// - anything else (unquoted keys, trailing garbage, wrong dialect): a genuine
///   formatting fault. Coach a clean re-issue as valid JSON. This is the
///   negative control — a malformed call is NEVER silently accepted or
///   mislabeled as a recoverable truncation.
pub(super) fn parse_error_carrier_feedback(
    tool_name: &str,
    parse_error: &str,
) -> (String, &'static str) {
    if parse_error_is_truncation(parse_error) {
        (
            format!(
                "Tool '{tool_name}' arguments could NOT be parsed because the tool call was \
                 TRUNCATED mid-stream — the arguments JSON ended before it was complete. This \
                 is NOT a missing-parameter slip: you did author the arguments, but the \
                 response was cut off before they finished. Re-issue the call with shorter \
                 content, or split the change into several smaller calls so the arguments fit \
                 in one response."
            ),
            "arguments_truncated",
        )
    } else {
        (
            format!(
                "Tool '{tool_name}' arguments could NOT be parsed as valid JSON. Re-issue the \
                 call as one complete, well-formed JSON object with the required parameters."
            ),
            "arguments_malformed",
        )
    }
}

/// True when a streamed-argument `__parse_error` message describes a buffer that
/// ended mid-value — a cut-off stream, not a dialect error. Keys on the two
/// diagnostics the JSON and Harn text-tool parsers emit for an incomplete tail
/// (`serde_json`'s "EOF while parsing ..." and the text-tool "unexpected end of
/// input"), so a truncation is recognized regardless of which parser ran last.
pub(super) fn parse_error_is_truncation(parse_error: &str) -> bool {
    parse_error.contains("EOF while parsing") || parse_error.contains("unexpected end of input")
}

/// Diagnose syntactically complete calls whose argument object is empty.
///
/// Parsing and dispatch share the same registry-owned required-argument
/// contract. Surfacing its empty-object result here makes an argument-less call
/// retryable feedback on every text grammar instead of pin-dependent silence.
/// The call remains in the result: dispatch still owns authoritative refusal
/// and its typed schema-validation envelope.
pub(super) fn append_empty_required_arg_diagnostics(
    parsed: &mut serde_json::Value,
    calls: &[serde_json::Value],
    registry: Option<&VmValue>,
) -> Result<(), VmError> {
    let schemas = tools::collect_tool_schemas(registry, None);
    let diagnostics = calls
        .iter()
        .filter_map(|call| {
            let name = call.get("name")?.as_str()?;
            let arguments = call
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            // Non-empty calls belong solely to dispatch validation, where host
            // tools can enforce richer contracts (for example alternative
            // argument groups) than the portable registry can express.
            arguments
                .as_object()
                .filter(|object| object.is_empty())
                .and_then(|_| tools::validate_tool_args(name, &arguments, &schemas).err())
        })
        .collect::<Vec<_>>();
    let errors = parsed
        .get_mut("tool_parse_errors")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| {
            VmError::Runtime(
                "__host_agent_parse_tool_calls: std/llm/tool_parse returned no tool_parse_errors list"
                    .to_string(),
            )
        })?;
    for message in diagnostics {
        if !errors
            .iter()
            .any(|entry| entry.as_str() == Some(message.as_str()))
        {
            errors.push(serde_json::Value::String(message));
        }
    }
    Ok(())
}
