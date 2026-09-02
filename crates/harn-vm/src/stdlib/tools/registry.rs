use crate::stdlib::macros::harn_builtin;
use crate::value::{VmDictExt, VmError, VmValue};

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "tool_registry(info?: {name: string, version?: string, description?: string}?, components?: {schemas: dict}?, cli?: {commands: list}?) -> {_type: \"tool_registry\", tools: list, info?: {name: string, version?: string, description?: string}, components?: {schemas: dict}, cli?: {commands: list}}",
    category = "tools"
)]
fn tool_registry_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut registry = crate::value::DictMap::new();
    registry.put_str("_type", "tool_registry");
    registry.insert(
        crate::value::intern_key("tools"),
        VmValue::List(Vec::new().into()),
    );
    for (index, key) in [(0, "info"), (1, "components"), (2, "cli")] {
        if let Some(value) = args
            .get(index)
            .filter(|value| !matches!(value, VmValue::Nil))
        {
            registry.insert(crate::value::intern_key(key), value.clone());
        }
    }
    let registry = VmValue::dict(registry);
    crate::tool_registry::tool_registry_catalog(&registry).map_err(|error| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "tool_registry: {error}"
        ))))
    })?;
    Ok(registry)
}
