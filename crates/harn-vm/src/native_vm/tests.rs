use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn native_harness_dispatch_rejects_another_backend_before_output() {
    let chunk = crate::compile_source(
        "fn main(harness: Harness) { harness.stdio.print(\"native reached\") }",
    )
    .unwrap();
    let mut vm = crate::new_vm();
    vm.set_harness(crate::value::VmHarness::from_host(Arc::new(())));
    let error = vm.execute(&chunk).await.unwrap_err();
    assert!(
        error.to_string().contains("different runtime backend"),
        "{error}"
    );
    assert!(vm.output().is_empty());

    let mut vm = crate::new_vm();
    vm.set_harness(crate::harness::Harness::real());
    vm.execute(&chunk).await.unwrap();
    assert_eq!(vm.output(), "native reached");
}

#[tokio::test]
async fn native_mcp_builtins_reject_another_backend_before_transport() {
    let mut vm = crate::new_vm();
    crate::mcp::register_mcp_builtins(&mut vm);
    let VmValue::Harness(root) = crate::harness::Harness::real().into_vm_value() else {
        unreachable!()
    };
    let tools = root.sub_handle("tools").unwrap();
    let client = VmValue::mcp_client(crate::value::VmMcpClientHandle::from_host(
        "foreign".to_string(),
        Arc::new(()),
    ));
    for builtin in [
        "mcp_list_tools",
        "mcp_call",
        "mcp_server_info",
        "mcp_disconnect",
        "mcp_list_resources",
        "mcp_read_resource",
        "mcp_list_resource_templates",
        "mcp_list_prompts",
        "mcp_get_prompt",
    ] {
        let error = crate::harness_methods::runtime_methods()
            .call(&mut vm, &tools, builtin, &[client.clone()])
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("different runtime backend"),
            "{builtin}: {error}"
        );
    }
}

#[derive(Clone)]
struct RefusingRuntime {
    native: NativeExecutionState,
    gates: Arc<AtomicUsize>,
}

impl RefusingRuntime {
    fn gate(&self, name: &str) -> Result<(), VmError> {
        self.gates.fetch_add(1, Ordering::SeqCst);
        if name.starts_with("probe_denied") {
            Err(VmError::UndefinedBuiltin("required runtime refused".into()))
        } else {
            Ok(())
        }
    }
}

impl VmRuntime for RefusingRuntime {
    fn poll_source_dir(
        &self,
        source_dir: &mut Option<std::path::PathBuf>,
        cx: &mut std::task::Context<'_>,
        continuation: &mut dyn FnMut(&mut std::task::Context<'_>) -> std::task::Poll<()>,
    ) -> std::task::Poll<()> {
        self.native.poll_source_dir(source_dir, cx, continuation)
    }

    fn as_any(&self) -> &dyn Any {
        &self.native
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        &mut self.native
    }
    fn fork_child(&self) -> Arc<dyn VmRuntime> {
        Arc::new(self.clone())
    }
    fn builtin_bridge(&self) -> Option<&dyn crate::vm::VmBuiltinBridge> {
        self.native.builtin_bridge()
    }
    fn harness_methods(&self) -> &dyn crate::vm::HarnessMethods {
        self.native.harness_methods()
    }
    fn prepare_top_level(&mut self, execution_id: &crate::ExecutionId, new_execution: bool) {
        self.native.prepare_top_level(execution_id, new_execution);
    }
    fn top_level_scope(
        &self,
        execution_id: &crate::ExecutionId,
    ) -> Box<dyn crate::vm::VmExecutionScope> {
        self.native.top_level_scope(execution_id)
    }
    fn task_cleanup(&self, execution_id: &crate::ExecutionId) -> Arc<dyn crate::vm::VmTaskCleanup> {
        self.native.task_cleanup(execution_id)
    }
    fn subtask_scope(&self) -> Box<dyn crate::vm::VmExecutionScope> {
        self.native.subtask_scope()
    }
    fn poll_async_builtin(
        &self,
        cx: &mut std::task::Context<'_>,
        continuation: &mut dyn FnMut(&mut std::task::Context<'_>) -> std::task::Poll<()>,
    ) -> std::task::Poll<()> {
        self.native.poll_async_builtin(cx, continuation)
    }
    fn execute_timed<'a>(
        &'a self,
        vm: &'a mut Vm,
        entry: crate::vm::TopLevelEntry,
        deadline: std::time::Instant,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>> {
        self.native.execute_timed(vm, entry, deadline)
    }
    fn start_span(&self, kind: crate::vm::VmSpanKind, name: String) -> crate::vm::ScopeSpan {
        self.native.start_span(kind, name)
    }
    fn builtin_span(&self, name: &str) -> Option<crate::vm::ScopeSpan> {
        self.native.builtin_span(name)
    }
    fn finish_pipeline<'a>(
        &'a self,
        vm: &'a mut Vm,
        value: VmValue,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>> {
        self.native.finish_pipeline(vm, value)
    }
    fn clear_pipeline_finish(&self) {
        self.native.clear_pipeline_finish();
    }
    fn baseline(&self) -> Arc<dyn VmRuntimeBaseline> {
        Arc::new(RefusingBaseline {
            native: self.native.baseline(),
            gates: self.gates.clone(),
        })
    }
    fn requires_async_builtin_gate(&self, name: &str) -> bool {
        name.ends_with("_async")
    }

    fn rebind_execution_state(&self, vm: &mut Vm) {
        self.native.rebind_execution_state(vm);
    }

    fn set_source_dir(&self, dir: &Path) {
        self.native.set_source_dir(dir);
    }
    fn enter_source_dir(&self, dir: Option<&Path>) -> Option<std::path::PathBuf> {
        self.native.enter_source_dir(dir)
    }
    fn runtime_context_value(&self, vm: &Vm) -> VmValue {
        self.native.runtime_context_value(vm)
    }

    fn is_replay(&self) -> bool {
        self.native.is_replay()
    }

    fn warn(&self, category: &str, message: &str) {
        self.native.warn(category, message);
    }

    fn is_step_function(&self, name: &str) -> bool {
        self.native.is_step_function(name)
    }
    fn take_active_step(&self, frame_depth: usize) -> Option<crate::vm::step_context::ActiveStep> {
        self.native.take_active_step(frame_depth)
    }
    fn before_step<'a>(
        &self,
        vm: &'a mut Vm,
        closure: &'a crate::value::VmClosure,
        args: Vec<VmValue>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<VmValue>, VmError>> + Send + 'a>> {
        self.native.before_step(vm, closure, args)
    }
    fn finish_step<'a>(
        &self,
        vm: &'a mut Vm,
        step: crate::vm::step_context::ActiveStep,
        output: VmValue,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send + 'a>> {
        self.native.finish_step(vm, step, output)
    }
    fn is_tracked_function(&self, name: &str) -> bool {
        self.native.is_tracked_function(name)
    }
    fn enter_function(&self, name: &str, frame_depth: usize, args: &[VmValue]) {
        self.native.enter_function(name, frame_depth, args);
    }
    fn prune_frames(&self, frame_depth: usize) {
        self.native.prune_frames(frame_depth);
    }
    fn suspend_active_context(&self) -> crate::vm::step_context::ActiveContextGuard {
        self.native.suspend_active_context()
    }
    fn step_error_boundary(
        &self,
        error: VmError,
        frame_depth: usize,
    ) -> crate::vm::step_context::StepBoundaryDecision {
        self.native.step_error_boundary(error, frame_depth)
    }
    fn finish_skipped_step(&self, frame_depth: usize, message: String) {
        self.native.finish_skipped_step(frame_depth, message);
    }
    fn denied_builtin_family(
        &self,
        denied: std::collections::HashSet<String>,
    ) -> std::collections::HashSet<String> {
        self.native.denied_builtin_family(denied)
    }
    fn gate_preparation(
        &self,
        name: &str,
        contract: Option<&harn_builtin_meta::BuiltinContract>,
    ) -> Result<(), VmError> {
        self.native.gate_preparation(name, contract)
    }
    fn gate_sync_builtin(&self, name: &str, _args: &[VmValue]) -> Result<(), VmError> {
        self.gate(name)
    }
    fn gate_async_builtin<'a>(
        &'a self,
        name: &'a str,
        _args: &'a [VmValue],
    ) -> Pin<Box<dyn Future<Output = Result<BuiltinGateDecision, VmError>> + Send + 'a>> {
        Box::pin(async move { self.gate(name).map(|()| BuiltinGateDecision::Continue) })
    }
    fn gate_bridge_builtin(&self, name: &str) -> Result<(), VmError> {
        self.gate(name)
    }
    fn builtin_effects(&self, name: &str) -> Option<&'static [harn_builtin_meta::EffectSpec]> {
        self.native.builtin_effects(name)
    }
    fn capability_effects(
        &self,
        capability: harn_builtin_meta::CapabilityId,
        method: &str,
    ) -> Option<&'static [harn_builtin_meta::EffectSpec]> {
        self.native.capability_effects(capability, method)
    }
}

struct RefusingBaseline {
    native: Arc<dyn VmRuntimeBaseline>,
    gates: Arc<AtomicUsize>,
}

impl VmRuntimeBaseline for RefusingBaseline {
    fn instantiate(&self, source_dir: Option<&Path>) -> Arc<dyn VmRuntime> {
        let runtime = self.native.instantiate(source_dir);
        Arc::new(RefusingRuntime {
            native: runtime
                .as_any()
                .downcast_ref::<NativeExecutionState>()
                .unwrap()
                .clone(),
            gates: self.gates.clone(),
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn required_runtime_denial_reaches_every_builtin_dispatch_and_survives_forks() {
    let gates = Arc::new(AtomicUsize::new(0));
    let handlers = Arc::new(AtomicUsize::new(0));
    let mut vm = Vm::with_runtime(Arc::new(RefusingRuntime {
        native: NativeExecutionState::fresh(),
        gates: gates.clone(),
    }));
    for name in ["probe_denied", "probe_denied_async", "probe_allowed"] {
        let handlers = handlers.clone();
        vm.register_builtin(name, move |_, _| {
            handlers.fetch_add(1, Ordering::SeqCst);
            Ok(VmValue::Int(7))
        });
    }
    let id = crate::BuiltinId::from_name("probe_denied");
    let mut child = vm.child_vm();
    let mut baseline = vm.baseline().instantiate();

    let refused = |result: Result<VmValue, VmError>, expected_gates| {
        assert!(
            matches!(&result, Err(VmError::UndefinedBuiltin(message)) if message == "required runtime refused"),
            "dispatch must return the required runtime's refusal: {result:?}"
        );
        assert_eq!(gates.load(Ordering::SeqCst), expected_gates);
    };
    refused(vm.call_named_builtin("probe_denied", vec![]).await, 1);
    refused(
        vm.call_builtin_id_for_test(id, "probe_denied", vec![])
            .await,
        2,
    );
    refused(
        vm.try_call_sync_builtin_for_test(Some(id), "probe_denied", &[])
            .unwrap(),
        3,
    );
    refused(
        vm.try_call_stack_builtin_for_test(Some(id), "probe_denied", 0)
            .unwrap(),
        4,
    );
    refused(
        vm.call_callable_value(&VmValue::BuiltinRef("probe_denied".into()), &[])
            .await,
        5,
    );
    refused(
        vm.call_callable_value(&VmValue::builtin_ref_id(id, "probe_denied"), &[])
            .await,
        6,
    );
    refused(vm.call_named_builtin("probe_denied_async", vec![]).await, 7);
    refused(child.call_named_builtin("probe_denied", vec![]).await, 8);
    refused(baseline.call_named_builtin("probe_denied", vec![]).await, 9);
    assert_eq!(
        handlers.load(Ordering::SeqCst),
        0,
        "denied handlers never fired"
    );

    assert_eq!(
        vm.call_named_builtin("probe_allowed", vec![])
            .await
            .unwrap()
            .display(),
        "7"
    );
    assert_eq!(gates.load(Ordering::SeqCst), 10);
    assert_eq!(
        handlers.load(Ordering::SeqCst),
        1,
        "the permitted neighboring handler fired"
    );
}
