use std::{future::Future, pin::Pin};

use crate::value::{VmError, VmValue};

/// Pinned future returned by async builtin handlers.
pub type AsyncBuiltinFuture = Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send>>;

pub type SyncHandler = fn(&[VmValue], &mut String) -> Result<VmValue, VmError>;
pub type AsyncHandler = fn(super::AsyncBuiltinCtx, Vec<VmValue>) -> AsyncBuiltinFuture;

/// A runtime handler attached to a builtin's typed metadata.
#[derive(Debug, Clone, Copy)]
pub enum VmBuiltinHandler {
    Sync(SyncHandler),
    Async(AsyncHandler),
    /// A parser-only declaration with no runtime handler.
    None,
}

pub type VmBuiltinDef = harn_builtin_registry::BuiltinDef<VmBuiltinHandler>;

/// Every linked builtin contributes one typed definition to this registry.
#[linkme::distributed_slice]
pub static ALL_BUILTIN_DEFS: [&'static VmBuiltinDef];
