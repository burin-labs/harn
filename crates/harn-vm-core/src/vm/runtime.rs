//! Required runtime state for an interpreter and its derived instances.

use std::{any::Any, future::Future, path::Path, pin::Pin, sync::Arc};

use crate::value::{VmError, VmValue};

pub enum BuiltinGateDecision {
    Continue,
    Return(VmValue),
}

pub enum VmSpanKind {
    Pipeline,
    Import,
    Parallel,
    Spawn,
}

pub trait VmBuiltinBridge: Send + Sync {
    fn call_builtin<'a>(
        &'a self,
        name: &'a str,
        args: &'a [VmValue],
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>>;
}

/// Required execution services supplied by the backend that constructs a VM.
/// The interpreter has no implicit native backend or permissive policy default.
pub trait VmRuntime: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    /// Return a newly owned aggregate with the same configuration and inherited
    /// handles. This also supplies copy-on-write state when a retained runtime
    /// handle prevents exclusive access; returning the original Arc is invalid.
    fn fork_child(&self) -> Arc<dyn VmRuntime>;
    fn baseline(&self) -> Arc<dyn VmRuntimeBaseline>;
    fn builtin_bridge(&self) -> Option<&dyn VmBuiltinBridge>;
    fn harness_methods(&self) -> &dyn super::HarnessMethods;
    fn prepare_top_level(&mut self, execution_id: &crate::ExecutionId, new_execution: bool);
    fn top_level_scope(
        &self,
        execution_id: &crate::ExecutionId,
    ) -> Box<dyn super::VmExecutionScope>;
    fn subtask_scope(&self) -> Box<dyn super::VmExecutionScope>;
    /// Poll the continuation exactly once under the backend's builtin scope,
    /// restoring the caller's scope before returning its poll result.
    fn poll_async_builtin(
        &self,
        cx: &mut std::task::Context<'_>,
        continuation: &mut dyn FnMut(&mut std::task::Context<'_>) -> std::task::Poll<()>,
    ) -> std::task::Poll<()>;
    fn execute_timed<'a>(
        &'a self,
        vm: &'a mut super::Vm,
        entry: super::TopLevelEntry,
        deadline: std::time::Instant,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>>;
    /// Poll once under the saved source directory, retaining any updated scope
    /// in `source_dir` and restoring the caller's directory before returning.
    fn poll_source_dir(
        &self,
        source_dir: &mut Option<std::path::PathBuf>,
        cx: &mut std::task::Context<'_>,
        continuation: &mut dyn FnMut(&mut std::task::Context<'_>) -> std::task::Poll<()>,
    ) -> std::task::Poll<()>;
    fn task_cleanup(&self, execution_id: &crate::ExecutionId) -> Arc<dyn VmTaskCleanup>;
    fn start_span(&self, kind: VmSpanKind, name: String) -> super::ScopeSpan;
    fn builtin_span(&self, name: &str) -> Option<super::ScopeSpan>;
    fn finish_pipeline<'a>(
        &'a self,
        vm: &'a mut super::Vm,
        value: VmValue,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>>;
    fn clear_pipeline_finish(&self);
    fn rebind_execution_state(&self, vm: &mut super::Vm);
    fn set_source_dir(&self, dir: &Path);
    fn enter_source_dir(&self, dir: Option<&Path>) -> Option<std::path::PathBuf>;
    fn is_step_function(&self, name: &str) -> bool;
    fn runtime_context_value(&self, vm: &super::Vm) -> VmValue;
    fn is_replay(&self) -> bool;
    fn warn(&self, category: &str, message: &str);
    fn take_active_step(&self, frame_depth: usize) -> Option<super::step_context::ActiveStep>;
    fn before_step<'a>(
        &self,
        vm: &'a mut super::Vm,
        closure: &'a crate::value::VmClosure,
        args: Vec<VmValue>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<VmValue>, VmError>> + Send + 'a>>;
    fn finish_step<'a>(
        &self,
        vm: &'a mut super::Vm,
        step: super::step_context::ActiveStep,
        output: VmValue,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>>;
    fn is_tracked_function(&self, name: &str) -> bool;
    fn enter_function(&self, name: &str, frame_depth: usize, args: &[VmValue]);
    fn prune_frames(&self, frame_depth: usize);
    fn suspend_active_context(&self) -> super::step_context::ActiveContextGuard;
    fn step_error_boundary(
        &self,
        error: VmError,
        frame_depth: usize,
    ) -> super::step_context::StepBoundaryDecision;
    fn finish_skipped_step(&self, frame_depth: usize, message: String);
    fn denied_builtin_family(
        &self,
        denied: std::collections::HashSet<String>,
    ) -> std::collections::HashSet<String>;
    fn requires_async_builtin_gate(&self, name: &str) -> bool;
    fn gate_preparation(
        &self,
        name: &str,
        contract: Option<&harn_builtin_meta::BuiltinContract>,
    ) -> Result<(), VmError>;
    fn gate_sync_builtin(&self, name: &str, args: &[VmValue]) -> Result<(), VmError>;
    fn gate_async_builtin<'a>(
        &'a self,
        name: &'a str,
        args: &'a [VmValue],
    ) -> Pin<Box<dyn Future<Output = Result<BuiltinGateDecision, VmError>> + Send + 'a>>;
    fn gate_bridge_builtin(&self, name: &str) -> Result<(), VmError>;
    fn builtin_effects(&self, name: &str) -> Option<&'static [harn_builtin_meta::EffectSpec]>;
    /// Run-tape hook before a builtin runs. `Some` answers the call in its
    /// place: a recorded read a branch replays, or the refusal of an effect
    /// a branch prefix must not repeat. `effects: None` is a pure builtin.
    fn serve_builtin_call(
        &self,
        _name: &str,
        _effects: Option<&'static [harn_builtin_meta::EffectSpec]>,
        _args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        None
    }
    /// True while a builtin call may be served from or recorded on a tape;
    /// false lets callers skip the tape hooks entirely.
    fn tapes_builtin_calls(&self) -> bool {
        false
    }
    /// True while builtin results may need recording, so callers keep the
    /// arguments for [`Self::record_builtin_call`].
    fn records_builtin_calls(&self) -> bool {
        false
    }
    /// Run-tape hook after a builtin ran.
    fn record_builtin_call(
        &self,
        _name: &str,
        _effects: Option<&'static [harn_builtin_meta::EffectSpec]>,
        _args: &[VmValue],
        _result: &Result<VmValue, VmError>,
    ) {
    }
    fn capability_effects(
        &self,
        capability: harn_builtin_meta::CapabilityId,
        method: &str,
    ) -> Option<&'static [harn_builtin_meta::EffectSpec]>;
}

pub trait VmRuntimeBaseline: Send + Sync {
    fn instantiate(&self, source_dir: Option<&Path>) -> Arc<dyn VmRuntime>;
}

/// Retained cleanup ownership for one execution, independent of later scopes.
pub trait VmTaskCleanup: Send + Sync {
    fn abandon_task<'a>(
        &'a self,
        task_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), VmError>> + Send + 'a>>;

    fn schedule(&self, task_id: String, before_cleanup: Pin<Box<dyn Future<Output = ()> + Send>>);
}
