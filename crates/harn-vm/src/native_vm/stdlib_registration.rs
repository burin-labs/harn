//! Immutable standard-library registration, separate from invocation state.
//!
//! A server initializes many independent VMs on one thread. Register handlers
//! and metadata once on that thread, then share their copy-on-write tables. The
//! snapshot contains no Harness, source identity, policy, mocks, or task state.

use crate::vm::{Vm, VmBuiltinRegistration};
use std::cell::LazyCell;

thread_local! {
    // VmValue is thread-affine. Keep namespace constants on their creating
    // thread rather than extending their authority with an unsafe Send impl.
    static REGISTRATION: LazyCell<VmBuiltinRegistration> = LazyCell::new(|| {
        let mut vm = crate::new_vm();
        crate::stdlib::register_stdlib_bindings(&mut vm);
        vm.builtin_registration()
    });
}

pub(crate) fn install_shared_stdlib_registration(vm: &mut Vm) -> bool {
    vm.install_builtin_registration(|| REGISTRATION.with(|cached| (**cached).clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::VmValue;
    use std::sync::Arc;

    #[tokio::test(flavor = "current_thread")]
    async fn standard_library_shares_bindings_but_not_invocation_state() {
        let mut first = crate::new_vm();
        let mut sibling = crate::new_vm();
        crate::stdlib::register_vm_stdlib(&mut first);
        crate::stdlib::register_vm_stdlib(&mut sibling);
        assert!(
            first.builtin_names().len() > 100,
            "empty registry is not sharing"
        );
        assert!(first.shares_builtin_tables_for_test(&sibling));
        assert!(!Arc::ptr_eq(
            first.synchronization(),
            sibling.synchronization()
        ));
        assert!(!Arc::ptr_eq(first.shared_state(), sibling.shared_state()));
        assert!(!first
            .harness()
            .unwrap()
            .shares_owner(sibling.harness().unwrap()));

        // The handler is actually reached on each VM, before and after a host
        // override forces one registration table to detach from the cache.
        let args = vec![VmValue::Int(-7)];
        assert!(matches!(
            first.call_named_builtin("abs", args.clone()).await.unwrap(),
            VmValue::Int(7)
        ));
        first.register_builtin("abs", |_, _| Ok(VmValue::Int(99)));
        assert!(matches!(
            first.call_named_builtin("abs", args.clone()).await.unwrap(),
            VmValue::Int(99)
        ));
        assert!(matches!(
            sibling
                .call_named_builtin("abs", args.clone())
                .await
                .unwrap(),
            VmValue::Int(7)
        ));
        let mut later = crate::new_vm();
        crate::stdlib::register_vm_stdlib(&mut later);
        assert!(matches!(
            later.call_named_builtin("abs", args).await.unwrap(),
            VmValue::Int(7)
        ));

        first.set_global("pi", VmValue::Int(0));
        assert!(matches!(sibling.global("pi"), Some(VmValue::Float(_))));
        assert!(matches!(later.global("pi"), Some(VmValue::Float(_))));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preinstalled_host_bindings_keep_ordered_registration() {
        let mut vm = crate::new_vm();
        vm.register_builtin("host_custom", |_, _| Ok(VmValue::Int(42)));
        vm.set_global("host_fact", VmValue::Int(13));
        assert!(!install_shared_stdlib_registration(&mut vm));
        crate::stdlib::register_vm_stdlib(&mut vm);
        assert!(matches!(
            vm.call_named_builtin("host_custom", vec![]).await.unwrap(),
            VmValue::Int(42)
        ));
        assert!(matches!(
            vm.call_named_builtin("abs", vec![VmValue::Int(-7)])
                .await
                .unwrap(),
            VmValue::Int(7)
        ));
        assert!(matches!(vm.global("host_fact"), Some(VmValue::Int(13))));
    }

    #[test]
    fn namespaces_cannot_capture_execution_objects() {
        let mut vm = crate::new_vm();
        vm.set_global("constant", VmValue::Float(1.0));
        let _ = vm.builtin_registration();
        vm.set_global("authority", crate::Harness::null().into_vm_value());
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            vm.builtin_registration()
        }))
        .is_err());
    }
}
