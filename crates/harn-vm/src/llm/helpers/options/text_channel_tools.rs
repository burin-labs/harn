use super::*;

/// Refuse a direct call whose tools would never reach the model.
///
/// A text-channel tool format sends no tool schemas: the model learns the
/// call grammar only from the contract `agent_loop` renders into the system
/// prompt (`std/agent/preflight` marks those calls). A direct call would
/// reach the wire with nothing tool-related on it, and the model would
/// answer without calling anything. Refuse instead of sending that. A
/// native tool-search meta-tool is on the wire, so it is not this case.
pub(super) fn refuse_dropped_text_tools(
    options: &Option<crate::value::DictMap>,
    tool_format: &str,
    native_tools_on_wire: bool,
    route_supports_native_tools: bool,
    provider: &str,
    model: &str,
) -> Result<(), VmError> {
    let tools = options.as_ref().and_then(|o| o.get("tools"));
    if tool_format == "native"
        || !tools_value_has_entries(tools)
        || native_tools_on_wire
        || opt_bool(options, "_tool_contract_rendered")
    {
        return Ok(());
    }
    Err(crate::llm::call::invalid_request_error(
        format!(
            "`tools` with tool_format `{tool_format}` needs the tool-call contract that \
             only `agent_loop` renders; a direct call would send no tools to `{model}` \
             (provider `{provider}`). Drive the tools through `agent_loop`, or pass \
             `tool_format: \"native\"`{native_hint}.",
            native_hint = if route_supports_native_tools {
                ""
            } else {
                " on a route that supports native tools"
            },
        ),
        provider,
        model,
    ))
}

/// Whether a `tools` option carries at least one tool: a non-empty list, or a
/// registry dict whose `tools` list is non-empty. An empty set sends nothing
/// either way, so it is not a dropped tool surface.
fn tools_value_has_entries(tools: Option<&VmValue>) -> bool {
    match tools {
        None | Some(VmValue::Nil) => false,
        Some(VmValue::List(items)) => !items.is_empty(),
        Some(VmValue::Dict(registry)) => match registry.get("tools") {
            Some(VmValue::List(items)) => !items.is_empty(),
            _ => true,
        },
        Some(_) => true,
    }
}
