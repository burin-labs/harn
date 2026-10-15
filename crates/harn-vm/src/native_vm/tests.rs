use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

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

    assert!(vm.call_named_builtin("probe_denied", vec![]).await.is_err());
    assert!(vm
        .call_builtin_id_or_name(id, "probe_denied", vec![])
        .await
        .is_err());
    assert!(vm
        .try_call_sync_builtin_id_or_name_args(
            Some(id),
            "probe_denied",
            &crate::vm::CallArgs::Slice(&[])
        )
        .unwrap()
        .is_err());
    assert!(vm
        .try_call_sync_builtin_id_or_name_from_stack_args(Some(id), "probe_denied", 0)
        .unwrap()
        .is_err());
    assert!(vm
        .call_callable_args(
            &VmValue::BuiltinRef("probe_denied".into()),
            crate::vm::CallArgs::Slice(&[])
        )
        .await
        .is_err());
    assert!(vm
        .call_callable_args(
            &VmValue::builtin_ref_id(id, "probe_denied"),
            crate::vm::CallArgs::Slice(&[])
        )
        .await
        .is_err());
    assert!(vm
        .call_named_builtin("probe_denied_async", vec![])
        .await
        .is_err());
    assert!(child
        .call_named_builtin("probe_denied", vec![])
        .await
        .is_err());
    assert!(baseline
        .call_named_builtin("probe_denied", vec![])
        .await
        .is_err());
    assert_eq!(
        gates.load(Ordering::SeqCst),
        9,
        "all dispatches reached the required runtime"
    );
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
