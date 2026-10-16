//! Runtime definitions referenced by generated builtin registrations.

pub use crate::value::{VmError, VmValue};
pub use crate::vm::{
    AsyncBuiltinCtx, AsyncBuiltinFuture, AsyncHandler, SyncHandler, VmBuiltinDef, VmBuiltinHandler,
    ALL_BUILTIN_DEFS,
};
pub use harn_builtin_meta::{
    shapes, BuiltinContract, BuiltinExposure, BuiltinSignature, CapabilityId, EffectAccess,
    EffectAuthorization, EffectKind, EffectSpec, Param, ResourceSelector, ShapeFieldDescriptor, Ty,
    TY_ANY, TY_BOOL, TY_BYTES, TY_BYTES_OR_NIL, TY_CLOSURE, TY_DICT, TY_DICT_OR_NIL, TY_DURATION,
    TY_FLOAT, TY_INT, TY_INT_OR_NIL, TY_LIST, TY_NEVER, TY_NIL, TY_NUMBER, TY_RESOURCE, TY_STRING,
    TY_STRING_OR_NIL,
};
pub use harn_builtin_registry::BuiltinDef;
pub use linkme;
pub use linkme::distributed_slice;

/// Install definitions and aliases in their declared order.
pub fn register_builtin_defs(vm: &mut crate::Vm, definitions: &'static [&'static VmBuiltinDef]) {
    for definition in definitions {
        vm.register_builtin_def(definition);
    }
}

/// Definitions declared in exactly one Rust module, indexed once from the
/// authoritative linked registry. Descendants are separate owners.
///
/// # Panics
/// Panics if the module has no linked definitions, preventing an empty install.
pub fn module_builtin_defs(module: &str) -> &'static [&'static VmBuiltinDef] {
    use std::collections::BTreeMap;
    use std::sync::OnceLock;

    static MODULES: OnceLock<BTreeMap<&'static str, Vec<&'static VmBuiltinDef>>> = OnceLock::new();
    MODULES
        .get_or_init(|| {
            let mut modules: BTreeMap<_, Vec<_>> = BTreeMap::new();
            for def in crate::vm::all_builtin_defs() {
                modules.entry(def.module).or_default().push(*def);
            }
            for definitions in modules.values_mut() {
                definitions.sort_by_key(|def| def.sig.name);
            }
            modules
        })
        .get(module)
        .unwrap_or_else(|| panic!("no linked builtin definitions for module `{module}`"))
}

/// Install a module's declarations without a parallel list of its members.
pub fn register_module_builtins(vm: &mut crate::Vm, module: &str) {
    register_builtin_defs(vm, module_builtin_defs(module));
}
