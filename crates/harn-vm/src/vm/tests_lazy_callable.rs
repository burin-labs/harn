use std::sync::Arc;

use crate::{LazyVmCallable, Vm, VmCallable, VmValue};

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
        VmCallable::Eager(_) => unreachable!("test callable is lazy"),
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
