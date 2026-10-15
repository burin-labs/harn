mod async_builtin;
mod builtin;
mod cache;
mod call_args;
mod callable_entry;
mod debug;
mod dispatch;
mod execution;
mod execution_deadline;
mod execution_evidence;
mod format;
mod harness_seam;
mod interrupts;
mod introspection;
pub mod iter;
mod methods;
mod module_phase_timing;
mod modules;
pub(crate) mod ops;
mod process_exit;
mod runtime;
mod runtime_scope;
mod scope;
mod state;
mod stdlib_artifact;
mod stdlib_export;
mod stdlib_registration;
pub(crate) mod step_context;
pub mod subtask;
mod task_cleanup;
mod tool_callable;
mod work;

#[cfg(test)]
mod depth_regression_tests;
#[cfg(test)]
mod tests_debug;
#[cfg(test)]
mod tests_lazy_callable;
#[cfg(test)]
mod tests_lexical_block;
#[cfg(test)]
mod tests_lexical_capture;
#[cfg(test)]
mod tests_package_runtime;
#[cfg(test)]
mod tests_runtime;
#[cfg(test)]
mod tests_runtime_process_exit;
#[cfg(test)]
mod tests_typed_op_fallback;
#[cfg(test)]
mod tests_value_calls;

pub(crate) use async_builtin::run_async_builtin_with;
pub use async_builtin::{AsyncBuiltinCtx, VmCallSite};
pub use builtin::{VmBuiltinArity, VmBuiltinKind, VmBuiltinMetadata};
pub(crate) use callable_entry::TopLevelEntry;
pub use debug::{DebugAction, DebugState};
pub(crate) use harness_seam::{HarnessMethodFuture, HarnessMethods};
pub use module_phase_timing::{ModulePhaseRecorder, ModulePhaseStats};
pub use modules::resolve_module_import_path;
pub(crate) use runtime::{
    BuiltinGateDecision, VmBuiltinBridge, VmRuntime, VmRuntimeBaseline, VmSpanKind, VmTaskCleanup,
};
pub(crate) use runtime_scope::{scope_runtime, VmExecutionScope};
pub use state::{Vm, VmBaseline};
pub(crate) use stdlib_artifact::prepare_stdlib_module_artifact;
pub use stdlib_artifact::{warm_embedded_stdlib, StdlibWarmReport};
#[cfg(test)]
pub(crate) use stdlib_export::compile_one as compile_stdlib_module_artifact;
pub use stdlib_export::{
    compile_embedded_stdlib_artifacts, CompiledStdlibArtifact, StdlibArtifactBuild,
};
pub(crate) use task_cleanup::{PendingTaskCleanup, SpawnedTask};
pub use work::{VmWork, VmWorkRecorder};

pub(crate) use call_args::CallArgs;
pub(crate) use ops::{CaughtError, ExceptionHandler};
pub(crate) use state::{
    CallFrame, InterruptHandler, IterState, LocalSlot, ScopeSpan, TaskScope, VmBuiltinDispatch,
    VmBuiltinEntry,
};
