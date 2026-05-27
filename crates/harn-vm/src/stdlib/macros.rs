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
