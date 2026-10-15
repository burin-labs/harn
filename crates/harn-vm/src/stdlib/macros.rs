//! Builtin registration imports for the native standard library.
//!
//! Generated definitions refer directly to the core runtime support module,
//! so a builtin's owning crate does not need a native stdlib module layout.

pub use harn_builtin_macros::{harn_builtin, harn_capability_method};
pub use harn_vm_core::builtin_support::*;

/// Register builtin definitions through the Core registry owner.
pub fn register_builtin_defs(vm: &mut crate::vm::Vm, defs: &'static [&'static VmBuiltinDef]) {
    harn_vm_core::builtin_support::register_builtin_defs(vm, defs);
}
