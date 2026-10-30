//! Execution fixture for private interpreter mechanics tests.
//! Native capabilities, timed host entries and step hooks fail explicitly.

use super::runtime::*;
use crate::{VmError, VmValue};
use std::{any::Any, future::Future, path::Path, pin::Pin, sync::Arc};

#[derive(Clone, Default)]
struct TestRuntime {
    execution: Option<crate::ExecutionId>,
}

pub(crate) fn new_vm() -> super::Vm {
    let mut vm = super::Vm::with_runtime(Arc::new(TestRuntime::default()));
    vm.set_harness(crate::value::VmHarness::from_host(Arc::new(())));
    vm
}

fn unsupported() -> VmError {
    VmError::Runtime("core test runtime has no native service".into())
}

struct TestScope(Option<crate::ExecutionId>);
impl super::VmExecutionScope for TestScope {
    fn poll(
        &mut self,
        cx: &mut std::task::Context<'_>,
        continuation: &mut dyn FnMut(&mut std::task::Context<'_>) -> std::task::Poll<()>,
    ) -> std::task::Poll<()> {
        let _scope = self.0.clone().map(crate::enter_execution_scope);
        continuation(cx)
    }
}

impl VmRuntime for TestRuntime {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn fork_child(&self) -> Arc<dyn VmRuntime> {
        Arc::new(self.clone())
    }
    fn baseline(&self) -> Arc<dyn VmRuntimeBaseline> {
        Arc::new(Self::default())
    }
    fn builtin_bridge(&self) -> Option<&dyn VmBuiltinBridge> {
        None
    }
    fn harness_methods(&self) -> &dyn super::HarnessMethods {
        self
    }
    fn prepare_top_level(&mut self, execution_id: &crate::ExecutionId, _new_execution: bool) {
        self.execution = Some(execution_id.clone());
    }
    fn top_level_scope(
        &self,
        execution_id: &crate::ExecutionId,
    ) -> Box<dyn super::VmExecutionScope> {
        Box::new(TestScope(Some(execution_id.clone())))
    }
    fn subtask_scope(&self) -> Box<dyn super::VmExecutionScope> {
        Box::new(TestScope(self.execution.clone()))
    }
    fn poll_async_builtin(
        &self,
        cx: &mut std::task::Context<'_>,
        continuation: &mut dyn FnMut(&mut std::task::Context<'_>) -> std::task::Poll<()>,
    ) -> std::task::Poll<()> {
        continuation(cx)
    }
    fn execute_timed<'a>(
        &'a self,
        _vm: &'a mut super::Vm,
        _entry: super::TopLevelEntry,
        _deadline: std::time::Instant,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>> {
        Box::pin(async { Err(unsupported()) })
    }
    fn poll_source_dir(
        &self,
        _source_dir: &mut Option<std::path::PathBuf>,
        cx: &mut std::task::Context<'_>,
        continuation: &mut dyn FnMut(&mut std::task::Context<'_>) -> std::task::Poll<()>,
    ) -> std::task::Poll<()> {
        continuation(cx)
    }
    fn task_cleanup(&self, _execution_id: &crate::ExecutionId) -> Arc<dyn VmTaskCleanup> {
        Arc::new(self.clone())
    }
    fn start_span(&self, _kind: VmSpanKind, _name: String) -> super::ScopeSpan {
        super::ScopeSpan::new(0, |id| assert_eq!(id, 0))
    }
    fn builtin_span(&self, _name: &str) -> Option<super::ScopeSpan> {
        None
    }
    fn finish_pipeline<'a>(
        &'a self,
        _vm: &'a mut super::Vm,
        value: VmValue,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>> {
        Box::pin(async move { Ok(value) })
    }
    fn clear_pipeline_finish(&self) {}
    fn rebind_execution_state(&self, _vm: &mut super::Vm) {}
    fn set_source_dir(&self, _dir: &Path) {}
    fn enter_source_dir(&self, _dir: Option<&Path>) -> Option<std::path::PathBuf> {
        None
    }
    fn is_step_function(&self, _name: &str) -> bool {
        false
    }
    fn runtime_context_value(&self, _vm: &super::Vm) -> VmValue {
        panic!("runtime context reporting requires a host runtime")
    }
    fn is_replay(&self) -> bool {
        false
    }
    fn warn(&self, category: &str, message: &str) {
        panic!("unexpected runtime warning: {category}: {message}")
    }
    fn take_active_step(&self, _frame_depth: usize) -> Option<super::step_context::ActiveStep> {
        None
    }
    fn before_step<'a>(
        &self,
        _vm: &'a mut super::Vm,
        _closure: &'a crate::value::VmClosure,
        _args: Vec<VmValue>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<VmValue>, VmError>> + Send + 'a>> {
        Box::pin(async { Err(unsupported()) })
    }
    fn finish_step<'a>(
        &self,
        _vm: &'a mut super::Vm,
        _step: super::step_context::ActiveStep,
        _output: VmValue,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>> {
        Box::pin(async { Err(unsupported()) })
    }
    fn is_tracked_function(&self, _name: &str) -> bool {
        false
    }
    fn enter_function(&self, _name: &str, _frame_depth: usize, _args: &[VmValue]) {}
    fn prune_frames(&self, _frame_depth: usize) {}
    fn suspend_active_context(&self) -> super::step_context::ActiveContextGuard {
        super::step_context::ActiveContextGuard::new(0, Default::default(), |id, outer| {
            assert_eq!(id, 0);
            assert!(outer.steps.is_empty());
            assert!(outer.personas.is_empty());
        })
    }
    fn step_error_boundary(
        &self,
        error: VmError,
        _frame_depth: usize,
    ) -> super::step_context::StepBoundaryDecision {
        super::step_context::StepBoundaryDecision::Throw(error)
    }
    fn finish_skipped_step(&self, _frame_depth: usize, _message: String) {
        panic!("step completion requires a host runtime")
    }
    fn denied_builtin_family(
        &self,
        denied: std::collections::HashSet<String>,
    ) -> std::collections::HashSet<String> {
        denied
    }
    fn requires_async_builtin_gate(&self, _name: &str) -> bool {
        false
    }
    fn gate_preparation(
        &self,
        _name: &str,
        _contract: Option<&harn_builtin_meta::BuiltinContract>,
    ) -> Result<(), VmError> {
        Ok(())
    }
    fn gate_sync_builtin(&self, _name: &str, _args: &[VmValue]) -> Result<(), VmError> {
        Ok(())
    }
    fn gate_async_builtin<'a>(
        &'a self,
        _name: &'a str,
        _args: &'a [VmValue],
    ) -> Pin<Box<dyn Future<Output = Result<BuiltinGateDecision, VmError>> + Send + 'a>> {
        Box::pin(async { Ok(BuiltinGateDecision::Continue) })
    }
    fn gate_bridge_builtin(&self, _name: &str) -> Result<(), VmError> {
        Err(unsupported())
    }
    fn builtin_effects(&self, _name: &str) -> Option<&'static [harn_builtin_meta::EffectSpec]> {
        None
    }
    fn capability_effects(
        &self,
        _capability: harn_builtin_meta::CapabilityId,
        _method: &str,
    ) -> Option<&'static [harn_builtin_meta::EffectSpec]> {
        None
    }
}
impl VmRuntimeBaseline for TestRuntime {
    fn instantiate(&self, _source_dir: Option<&Path>) -> Arc<dyn VmRuntime> {
        Arc::new(Self::default())
    }
}
impl super::HarnessMethods for TestRuntime {
    fn call<'a>(
        &self,
        _vm: &'a mut super::Vm,
        _handle: &'a crate::value::VmHarness,
        _method: &'a str,
        _args: &'a [VmValue],
    ) -> super::HarnessMethodFuture<'a> {
        Box::pin(async { Err(unsupported()) })
    }
    fn call_sync_fast(
        &self,
        _output: &mut String,
        _effects: &mut crate::runtime_effects::RuntimeEffectState,
        _handle: &crate::value::VmHarness,
        _method: &str,
        _args: &[VmValue],
    ) -> Option<Result<VmValue, VmError>> {
        Some(Err(unsupported()))
    }
}
impl VmTaskCleanup for TestRuntime {
    fn abandon_task<'a>(
        &'a self,
        _task_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), VmError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
    // The fixture carries no ambient scope to lose.
    #[allow(clippy::disallowed_methods)]
    fn schedule(&self, _task_id: String, before_cleanup: Pin<Box<dyn Future<Output = ()> + Send>>) {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(before_cleanup);
        }
    }
}
