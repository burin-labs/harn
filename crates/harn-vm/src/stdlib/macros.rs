//! Support module referenced by `#[harn_builtin]`-emitted code.
//!
//! The proc-macro emits paths like `crate::stdlib::macros::VmBuiltinDef`,
//! `crate::stdlib::macros::BuiltinSignature`, etc. This module re-exports
//! everything those expansions need so any `harn-vm/src/stdlib/*.rs` file
//! can apply `#[harn_builtin]` to a fn without extra imports.

use std::future::Future;
use std::pin::Pin;

pub use crate::value::{VmError, VmValue};

pub use harn_builtin_macros::harn_builtin;
pub use harn_builtin_meta::{
    BuiltinSignature, Param, ShapeFieldDescriptor, Ty, TY_ANY, TY_BOOL, TY_BYTES, TY_BYTES_OR_NIL,
    TY_CLOSURE, TY_DICT, TY_DICT_OR_NIL, TY_DURATION, TY_FLOAT, TY_INT, TY_INT_OR_NIL, TY_LIST,
    TY_NEVER, TY_NIL, TY_NUMBER, TY_STRING, TY_STRING_OR_NIL,
};
pub use harn_builtin_registry::BuiltinDef;
// Re-export so the `#[harn_builtin]` proc-macro can name the
// distributed-slice attribute without each call-site importing linkme.
pub use linkme::distributed_slice;

/// Workspace-global registry of `#[harn_builtin]`-emitted definitions.
/// Each annotated fn contributes one entry via
/// `#[linkme::distributed_slice(ALL_BUILTIN_DEFS)]`, so there is no
/// per-module `MODULE_BUILTINS` slice to maintain. The CLI / LSP / lint /
/// serve / dap binaries force-link `harn-vm` to defeat rlib dead-code
/// stripping (linkme issue #36) so every static lands in this slice at
/// link time.
#[linkme::distributed_slice]
pub static ALL_BUILTIN_DEFS: [&'static VmBuiltinDef];

/// Pinned future returned by async builtin handlers.
pub type AsyncBuiltinFuture = Pin<Box<dyn Future<Output = Result<VmValue, VmError>>>>;

/// Sync builtin handler signature (matches `crate::vm::dispatch`'s register_builtin shape).
pub type SyncHandler = fn(&[VmValue], &mut String) -> Result<VmValue, VmError>;
/// Async builtin handler signature.
pub type AsyncHandler = fn(Vec<VmValue>) -> AsyncBuiltinFuture;

/// Runtime handler attached to a `VmBuiltinDef`. `None` covers parser-only
/// builtins (method-dispatched at runtime, but the parser still wants a
/// signature for typo suggestion + return-type inference).
#[derive(Debug, Clone, Copy)]
pub enum VmBuiltinHandler {
    Sync(SyncHandler),
    Async(AsyncHandler),
    /// No runtime impl — registered with the parser only (e.g. `len`,
    /// `split`, method-dispatch builtins).
    None,
}

/// `BuiltinDef` specialized to the VM's handler type.
pub type VmBuiltinDef = BuiltinDef<VmBuiltinHandler>;

/// Eager-registration helper: install every entry (name + aliases) on `vm`
/// using the macro-emitted handler. Each module's `register_*_builtins(vm)`
/// calls this with its `MODULE_BUILTINS` slice so the call ordering between
/// modules stays deterministic (e.g. `clock` overrides `process::timestamp`).
pub fn register_builtin_defs(vm: &mut crate::vm::Vm, defs: &'static [&'static VmBuiltinDef]) {
    for def in defs {
        vm.register_builtin_def(def);
    }
}

/// Deferred-registration helper: register names + aliases as deferred
/// builtins on `vm`. The first time any of them dispatches, `registrar`
/// runs (typically installs the LLM stack), which re-registers the real
/// impls.
pub fn register_deferred_builtin_defs(
    vm: &mut crate::vm::Vm,
    defs: &[&'static VmBuiltinDef],
    registrar: fn(&mut crate::vm::Vm),
) {
    for def in defs {
        vm.register_deferred_builtin(def.sig.name, registrar);
        for alias in def.aliases {
            vm.register_deferred_builtin(alias, registrar);
        }
    }
}
