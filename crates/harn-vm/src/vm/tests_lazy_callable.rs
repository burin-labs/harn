use std::sync::Arc;

use crate::{LazyPipelineCallable, LazyVmCallable, Vm, VmCallable, VmValue};

#[test]
fn lazy_callable_reuses_one_vm_module_state_but_isolates_fresh_vms() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime builds");
    let dir = tempfile::tempdir().expect("tempdir");
    let module_path = dir.path().join("counter.harn");
    std::fs::write(
        &module_path,
        r"
let counter = 0

pub fn next() -> int {
  counter = counter + 1
  return counter
}

pub fn current() -> int {
  return counter
}
",
    )
    .expect("write module");
    let callable = VmCallable::Lazy(LazyVmCallable::new(module_path, "next"));
    let current = match &callable {
        VmCallable::Lazy(lazy) => {
            VmCallable::Lazy(LazyVmCallable::new(lazy.module_path.clone(), "current"))
        }
        VmCallable::Eager(_) | VmCallable::Pipeline(_) => unreachable!("test callable is lazy"),
    };

    runtime.block_on(async {
        let root = Vm::new();
        let mut first = root.child_vm();
        let first_call = first
            .resolve_callable(&callable)
            .await
            .expect("first VM resolves callable");
        let first_value = first
            .call_closure_pub(&first_call, &[])
            .await
            .expect("first call succeeds");
        assert!(matches!(first_value, VmValue::Int(1)), "{first_value:?}");
        let mut second = root.child_vm();
        let second_call = second
            .resolve_callable(&callable)
            .await
            .expect("second child resolves callable");
        assert!(Arc::ptr_eq(&first_call, &second_call));
        let second_value = second
            .call_closure_pub(&second_call, &[])
            .await
            .expect("second call succeeds");
        assert!(matches!(second_value, VmValue::Int(2)), "{second_value:?}");

        let mut third = root.child_vm();
        let current_call = third
            .resolve_callable(&current)
            .await
            .expect("other export resolves from shared module");
        let current_value = third
            .call_closure_pub(&current_call, &[])
            .await
            .expect("other export observes shared state");
        assert!(
            matches!(current_value, VmValue::Int(2)),
            "{current_value:?}"
        );

        let mut fresh = Vm::new();
        let fresh_call = fresh
            .resolve_callable(&callable)
            .await
            .expect("fresh VM resolves callable");
        assert!(!Arc::ptr_eq(&first_call, &fresh_call));
        let fresh_value = fresh
            .call_closure_pub(&fresh_call, &[])
            .await
            .expect("fresh VM call succeeds");
        assert!(matches!(fresh_value, VmValue::Int(1)), "{fresh_value:?}");
    });
}

#[tokio::test(flavor = "current_thread")]
async fn lazy_pipeline_callable_binds_arguments_and_returns_value() {
    let dir = tempfile::tempdir().expect("tempdir");
    let caller_dir = tempfile::tempdir().expect("caller tempdir");
    let module_path = dir.path().join("workflow.harn");
    std::fs::write(
        &module_path,
        r"
pub pipeline run(value) {
  return {received: value}
}
",
    )
    .expect("write pipeline module");
    let callable = VmCallable::Pipeline(LazyPipelineCallable::new(module_path, "run"));
    let mut vm = Vm::new();
    vm.set_source_dir(caller_dir.path());

    let result = vm
        .execute_callable(&callable, &[VmValue::Int(42)])
        .await
        .expect("pipeline executes");

    let VmValue::Dict(result) = result else {
        panic!("expected pipeline result dict, got {result:?}");
    };
    assert!(matches!(result.get("received"), Some(VmValue::Int(42))));
    assert_eq!(vm.source_dir.as_deref(), Some(caller_dir.path()));
}

#[tokio::test(flavor = "current_thread")]
async fn lazy_pipeline_callable_rejects_wrong_typed_argument() {
    let dir = tempfile::tempdir().expect("tempdir");
    let module_path = dir.path().join("workflow.harn");
    std::fs::write(
        &module_path,
        r"
pub pipeline run(value: int) -> int {
  return value
}
",
    )
    .expect("write pipeline module");
    let callable = VmCallable::Pipeline(LazyPipelineCallable::new(module_path, "run"));
    let error = Vm::new()
        .execute_callable(&callable, &[VmValue::String("wrong".into())])
        .await
        .expect_err("typed pipeline rejects the wrong runtime argument");

    assert_eq!(
        error.to_string(),
        "Runtime error: TypeError: parameter 'value' expected int, got string (wrong)"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn lazy_pipeline_callable_resolves_module_type_alias_guard() {
    let dir = tempfile::tempdir().expect("tempdir");
    let module_path = dir.path().join("workflow.harn");
    std::fs::write(
        &module_path,
        r"
type Count = int

pub pipeline run(value: Count) -> int {
  return value
}
",
    )
    .expect("write pipeline module");
    let callable = VmCallable::Pipeline(LazyPipelineCallable::new(module_path, "run"));
    let error = Vm::new()
        .execute_callable(&callable, &[VmValue::String("wrong".into())])
        .await
        .expect_err("module alias guards the exported pipeline argument");

    assert_eq!(
        error.to_string(),
        "Runtime error: TypeError: parameter 'value' expected int, got string (wrong)"
    );
}
