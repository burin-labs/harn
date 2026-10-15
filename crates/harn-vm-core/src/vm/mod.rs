mod async_builtin;
mod builtin;
mod builtin_def;
mod builtin_manifest;
mod cache;
mod call_args;
mod callable_entry;
mod debug;
mod dispatch;
mod execution;
#[cfg(feature = "vm-test-support")]
#[doc(hidden)]
pub use execution::{reset_scope_interrupt_async_dispatches, scope_interrupt_async_dispatches};
mod execution_deadline;
mod execution_evidence;
pub mod flight_recorder;
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
mod registration;
mod runtime;
mod runtime_context;
mod runtime_scope;
mod scope;
mod state;
mod stdlib_artifact;
mod stdlib_export;
pub mod step_context;
pub mod subtask;
mod task_cleanup;
#[cfg(feature = "vm-test-support")]
mod test_support;
#[cfg(feature = "vm-test-support")]
#[doc(hidden)]
pub use test_support::{abort_task_for_test, VmDeadlineProbe};
#[cfg(feature = "vm-test-support")]
#[doc(hidden)]
pub use test_support::{
    compile_stdlib_source_for_test, module_interface_resolution_count_for_test,
    reset_stdlib_cache_for_test, stdlib_cached_ptr_for_test, stdlib_warm_lock_file_for_test,
};
mod tool_callable;
mod work;

#[cfg(any(test, feature = "vm-bench-internals"))]
pub(crate) mod test_runtime;

pub use async_builtin::run_async_builtin_with;
pub use async_builtin::{AsyncBuiltinCtx, VmCallSite};
pub use builtin::{VmBuiltinArity, VmBuiltinKind, VmBuiltinMetadata};
pub use builtin_def::{
    AsyncBuiltinFuture, AsyncHandler, SyncHandler, VmBuiltinDef, VmBuiltinHandler, ALL_BUILTIN_DEFS,
};
pub use builtin_manifest::builtin_policy_metadata;
pub use builtin_manifest::{
    all_builtin_defs, all_builtin_manifest, builtin_for_harness_path, builtin_manifest_entry,
    capability_method_manifest_entry, harness_method_for_builtin,
    recorded_effect_builtin_manifest_entry,
};
pub use callable_entry::TopLevelEntry;
pub use debug::{DebugAction, DebugState};
pub use execution_deadline::{ExecutionDeadlineGuard, ExecutionDeadlinePauseGuard};
pub use harness_seam::{HarnessMethodFuture, HarnessMethods};
pub use methods::Mutation;
pub use module_phase_timing::{ModulePhaseRecorder, ModulePhaseStats};
pub use modules::resolve_module_import_path;
pub use registration::VmBuiltinRegistration;
pub use runtime::{
    BuiltinGateDecision, VmBuiltinBridge, VmRuntime, VmRuntimeBaseline, VmSpanKind, VmTaskCleanup,
};
pub use runtime_context::RuntimeContext;
pub use runtime_context::{runtime_context_clear, runtime_context_get, runtime_context_set};
pub use runtime_scope::{scope_runtime, VmExecutionScope};
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
pub use state::ScopeSpan;
pub(crate) use state::{
    CallFrame, InterruptHandler, IterState, LocalSlot, TaskScope, VmBuiltinDispatch, VmBuiltinEntry,
};
