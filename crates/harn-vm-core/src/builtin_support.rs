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
