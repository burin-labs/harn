//! Immutable standard-library registration, separate from invocation state.
//!
//! A server initializes many independent VMs on one thread. Register handlers
//! and metadata once on that thread, then share their copy-on-write tables. The
//! snapshot contains no Harness, source identity, policy, mocks, or task state.

use std::cell::LazyCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::value::{DictMap, VmAsyncBuiltinFn, VmBuiltinFn, VmValue};
use crate::BuiltinId;

use super::{Vm, VmBuiltinDispatch, VmBuiltinEntry, VmBuiltinMetadata};

struct StdlibRegistration {
    builtins: Arc<BTreeMap<String, VmBuiltinFn>>,
    async_builtins: Arc<BTreeMap<String, VmAsyncBuiltinFn>>,
    capability_methods:
        Arc<BTreeMap<harn_builtin_meta::CapabilityId, BTreeMap<String, VmBuiltinDispatch>>>,
    builtin_metadata: Arc<BTreeMap<String, VmBuiltinMetadata>>,
    builtins_by_id: Arc<HashMap<BuiltinId, VmBuiltinEntry>>,
    builtin_id_collisions: Arc<HashSet<BuiltinId>>,
    globals: Arc<DictMap>,
}

thread_local! {
    // VmValue is thread-affine. Keep namespace constants on their creating
    // thread rather than extending their authority with an unsafe Send impl.
    static REGISTRATION: LazyCell<StdlibRegistration> = LazyCell::new(|| {
        let mut vm = Vm::new();
        crate::stdlib::register_stdlib_bindings(&mut vm);
        for (name, value) in vm.globals.iter() {
            assert!(immutable_namespace_value(value),
                "stdlib registration global `{name}` contains invocation state");
        }
        assert!(vm.harness().is_none(), "stdlib bindings installed a Harness");
        StdlibRegistration {
            builtins: Arc::clone(&vm.builtins),
            async_builtins: Arc::clone(&vm.async_builtins),
            capability_methods: Arc::clone(&vm.capability_methods),
            builtin_metadata: Arc::clone(&vm.builtin_metadata),
            builtins_by_id: Arc::clone(&vm.builtins_by_id),
            builtin_id_collisions: Arc::clone(&vm.builtin_id_collisions),
            globals: Arc::clone(&vm.globals),
        }
    });
}

fn immutable_namespace_value(value: &VmValue) -> bool {
    match value {
        VmValue::Nil
        | VmValue::Bool(_)
        | VmValue::Int(_)
        | VmValue::Float(_)
        | VmValue::String(_)
        | VmValue::BuiltinRef(_) => true,
        VmValue::Dict(entries) => entries.values().all(immutable_namespace_value),
        _ => false,
    }
}

impl Vm {
    pub(crate) fn install_shared_stdlib_registration(&mut self) -> bool {
        // Hosts that register adapters first retain the ordered registration
        // behavior, including specialized capability handlers taking precedence.
        if !self.builtins.is_empty()
            || !self.async_builtins.is_empty()
            || !self.capability_methods.is_empty()
        {
            return false;
        }
        REGISTRATION.with(|registration| {
            self.builtins = Arc::clone(&registration.builtins);
            self.async_builtins = Arc::clone(&registration.async_builtins);
            self.capability_methods = Arc::clone(&registration.capability_methods);
            self.builtin_metadata = Arc::clone(&registration.builtin_metadata);
            self.builtins_by_id = Arc::clone(&registration.builtins_by_id);
            self.builtin_id_collisions = Arc::clone(&registration.builtin_id_collisions);
            if self.globals.is_empty() {
                self.globals = Arc::clone(&registration.globals);
            } else {
                Arc::make_mut(&mut self.globals).extend(
                    registration
                        .globals
                        .iter()
                        .map(|(name, value)| (name.clone(), value.clone())),
                );
            }
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn standard_library_shares_bindings_but_not_invocation_state() {
        let mut first = Vm::new();
        let mut sibling = Vm::new();
        crate::stdlib::register_vm_stdlib(&mut first);
        crate::stdlib::register_vm_stdlib(&mut sibling);
        assert!(first.builtins.len() > 100, "empty registry is not sharing");
        assert!(Arc::ptr_eq(&first.builtins, &sibling.builtins));
        assert!(Arc::ptr_eq(
            &first.builtin_metadata,
            &sibling.builtin_metadata
        ));
        assert!(!Arc::ptr_eq(&first.sync_runtime, &sibling.sync_runtime));
        assert!(!Arc::ptr_eq(
            &first.shared_state_runtime,
            &sibling.shared_state_runtime
        ));
        assert!(!Arc::ptr_eq(
            first.harness().unwrap().inner(),
            sibling.harness().unwrap().inner()
        ));

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
        let mut later = Vm::new();
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
        let mut vm = Vm::new();
        vm.register_builtin("host_custom", |_, _| Ok(VmValue::Int(42)));
        vm.set_global("host_fact", VmValue::Int(13));
        assert!(!vm.install_shared_stdlib_registration());
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
        assert!(immutable_namespace_value(&VmValue::Float(1.0)));
        let mut vm = Vm::new();
        crate::stdlib::register_vm_stdlib(&mut vm);
        assert!(!immutable_namespace_value(
            &vm.root_harness_value().unwrap()
        ));
    }
}
