//! Registry-owned diagnostics added at the public tool-parse boundary.

use crate::llm::tools;
use crate::value::{VmError, VmValue};

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
