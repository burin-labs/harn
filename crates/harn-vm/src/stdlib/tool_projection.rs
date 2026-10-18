use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_tool_projection_builtins(vm: &mut Vm) {
    crate::stdlib::macros::register_module_builtins(vm, module_path!());
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "tool_project(registry: {_type: \"tool_registry\", tools: list} | list, audience: \"cli\" | \"mcp\" | \"catalog\" | \"dashboard\" | \"agent\") -> {_type: \"tool_registry\", tools: list} | list",
    category = "tools"
)]
fn tool_project_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(registry) = args.first() else {
        return Err(VmError::Thrown(VmValue::string(
            "tool_project: requires a registry and audience",
        )));
    };
    let audience = match args.get(1) {
        Some(VmValue::String(value)) => crate::tool_registry::ToolAudience::parse(value),
        _ => None,
    }
    .ok_or_else(|| {
        VmError::Thrown(VmValue::string("tool_project: audience must be one of \"cli\", \"mcp\", \"catalog\", \"dashboard\", or \"agent\""))
    })?;
    crate::tool_registry::project_tools_for_audience(registry, audience)
        .map_err(|error| VmError::Thrown(VmValue::string(format!("tool_project: {error}"))))
}
