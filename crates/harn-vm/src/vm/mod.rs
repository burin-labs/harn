mod async_builtin;
mod builtin;
mod call_args;
mod debug;
mod dispatch;
mod execution;
mod format;
mod interrupts;
pub mod iter;
mod methods;
mod modules;
pub(crate) mod ops;
mod scope;
mod state;

#[cfg(test)]
mod depth_regression_tests;
#[cfg(test)]
mod tests_debug;
#[cfg(test)]
mod tests_runtime;

pub(crate) use async_builtin::run_async_builtin_with;
pub use async_builtin::AsyncBuiltinCtx;
pub use builtin::{VmBuiltinArity, VmBuiltinKind, VmBuiltinMetadata};
pub use debug::{DebugAction, DebugState};
pub use modules::resolve_module_import_path;
pub use state::{Vm, VmBaseline};

pub(crate) use call_args::CallArgs;
pub(crate) use state::{
    CallFrame, ExceptionHandler, InterruptHandler, IterState, LocalSlot, ScopeSpan,
    VmBuiltinDispatch, VmBuiltinEntry,
};
