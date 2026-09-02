use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_tool_projection_builtins(vm: &mut Vm) {
    register_builtin_defs(vm, MODULE_BUILTINS);
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "tool_project(registry: {_type: \"tool_registry\", tools: list} | list, audience: \"cli\" | \"mcp\" | \"catalog\" | \"dashboard\" | \"agent\") -> {_type: \"tool_registry\", tools: list} | list",
    category = "tools"
)]
fn tool_project_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(registry) = args.first() else {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "tool_project: requires a registry and audience",
        ))));
    };
    let audience = match args.get(1) {
        Some(VmValue::String(value)) => crate::tool_registry::ToolAudience::parse(value),
        _ => None,
    }
    .ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "tool_project: audience must be one of \"cli\", \"mcp\", \"catalog\", \"dashboard\", or \"agent\"",
        )))
    })?;
    crate::tool_registry::project_tools_for_audience(registry, audience).map_err(|error| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "tool_project: {error}"
        ))))
    })
}

const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&TOOL_PROJECT_IMPL_DEF];
