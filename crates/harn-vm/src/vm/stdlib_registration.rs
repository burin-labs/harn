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
