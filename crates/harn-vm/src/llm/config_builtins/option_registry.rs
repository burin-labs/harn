use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{DictMap, VmDictExt, VmError, VmValue};

pub(super) const OPTION_REGISTRY_DEFS: &[&VmBuiltinDef] = &[&LLM_CALL_OPTION_REGISTRY_BUILTIN_DEF];

/// Expose the canonical `llm_call` option registry to Harn so stdlib
/// projection and removed-key diagnostics cannot drift from the runtime.
#[harn_builtin(sig = "__llm_call_option_registry() -> dict", category = "llm.config")]
fn llm_call_option_registry_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let keys = harn_builtin_meta::llm_options::LLM_CALL_OPTION_FIELDS
        .iter()
        .map(|field| VmValue::String(field.name.into()))
        .collect();
    let wrapper_only = harn_builtin_meta::llm_options::LLM_WRAPPER_ONLY_KEYS
        .iter()
        .map(|key| VmValue::String((*key).into()))
        .collect();
    let mut removed = DictMap::new();
    for entry in harn_builtin_meta::llm_options::LLM_REMOVED_OPTIONS {
        removed.put_str(entry.key, entry.fix);
    }
    let mut registry = DictMap::new();
    registry.put("keys", VmValue::List(std::sync::Arc::new(keys)));
    registry.put(
        "wrapper_only",
        VmValue::List(std::sync::Arc::new(wrapper_only)),
    );
    registry.put("removed", VmValue::dict(removed));
    Ok(VmValue::dict(registry))
}
