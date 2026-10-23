use crate::stdlib::macros::harn_builtin;
use crate::value::{VmDictExt, VmError, VmValue};

pub(super) fn register(vm: &mut crate::vm::Vm) {
    crate::stdlib::macros::register_module_builtins(vm, module_path!());
}

#[harn_builtin(
    exposure = "pure",
    sig = "tool_invocation_binding() -> dict?",
    category = "tools"
)]
fn tool_invocation_binding_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(
        crate::tool_registry::preparation_scope::current_invocation()
            .map(|bound| crate::schema::json_to_vm_value(&bound.facts))
            .unwrap_or(VmValue::Nil),
    )
}

#[harn_builtin(
    exposure = "pure",
    sig = "tool_schema(registry: {_type: \"tool_registry\", tools: list} | closure) -> ToolCatalog",
    category = "tools"
)]
fn tool_schema_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let registry = args
        .first()
        .ok_or_else(|| VmError::Thrown(VmValue::string("tool_schema: requires a tool registry")))?;
    let schema = crate::tool_registry::tool_registry_schema(registry)?;
    Ok(crate::schema::json_to_vm_value(&schema))
}

#[harn_builtin(
    exposure = "pure",
    sig = "tool_registry_validate(registry: ToolRegistry) -> ToolRegistry",
    category = "tools"
)]
fn tool_registry_validate_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let registry = args.first().ok_or_else(|| {
        VmError::Runtime("tool_registry_validate: requires a tool registry".into())
    })?;
    let catalog = crate::tool_registry::tool_registry_catalog(registry)?;
    crate::tool_registry::PreparedToolCatalog::prepare(catalog)
        .map_err(|error| VmError::Runtime(format!("tool_registry_validate: {error}")))?;
    Ok(registry.clone())
}

#[harn_builtin(
    exposure = "pure",
    sig = "tool_registry(info?: {name: string, version?: string, description?: string}?, components?: {schemas: dict}?, cli?: {commands: list}?) -> {_type: \"tool_registry\", tools: list, info?: {name: string, version?: string, description?: string}, components?: {schemas: dict}, cli?: {commands: list}}",
    category = "tools"
)]
fn tool_registry_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut registry = crate::value::DictMap::new();
    registry.put_str("_type", "tool_registry");
    registry.put("tools", VmValue::List(Vec::new().into()));
    for (index, key) in [(0, "info"), (1, "components"), (2, "cli")] {
        if let Some(value) = args
            .get(index)
            .filter(|value| !matches!(value, VmValue::Nil))
        {
            registry.insert(crate::value::intern_key(key), value.clone());
        }
    }
    let registry = VmValue::dict(registry);
    crate::tool_registry::tool_registry_catalog(&registry)
        .map_err(|error| VmError::Thrown(VmValue::string(format!("tool_registry: {error}"))))?;
    Ok(registry)
}
