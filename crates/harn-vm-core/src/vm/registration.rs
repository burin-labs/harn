use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use crate::value::{DictMap, VmAsyncBuiltinFn, VmBuiltinFn, VmValue};
use crate::BuiltinId;

use super::{Vm, VmBuiltinDispatch, VmBuiltinEntry, VmBuiltinMetadata};

/// Handler tables and immutable namespace constants, without invocation state.
#[derive(Clone)]
pub struct VmBuiltinRegistration {
    builtins: Arc<BTreeMap<String, VmBuiltinFn>>,
    async_builtins: Arc<BTreeMap<String, VmAsyncBuiltinFn>>,
    capability_methods:
        Arc<BTreeMap<harn_builtin_meta::CapabilityId, BTreeMap<String, VmBuiltinDispatch>>>,
    builtin_metadata: Arc<BTreeMap<String, VmBuiltinMetadata>>,
    builtins_by_id: Arc<HashMap<BuiltinId, VmBuiltinEntry>>,
    builtin_id_collisions: Arc<HashSet<BuiltinId>>,
    globals: Arc<DictMap>,
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
    pub fn builtin_registration(&self) -> VmBuiltinRegistration {
        for (name, value) in self.globals.iter() {
            assert!(
                immutable_namespace_value(value),
                "stdlib registration global `{name}` contains invocation state"
            );
        }
        assert!(
            self.harness().is_none(),
            "stdlib bindings installed a Harness"
        );
        VmBuiltinRegistration {
            builtins: Arc::clone(&self.builtins),
            async_builtins: Arc::clone(&self.async_builtins),
            capability_methods: Arc::clone(&self.capability_methods),
            builtin_metadata: Arc::clone(&self.builtin_metadata),
            builtins_by_id: Arc::clone(&self.builtins_by_id),
            builtin_id_collisions: Arc::clone(&self.builtin_id_collisions),
            globals: Arc::clone(&self.globals),
        }
    }

    pub fn install_builtin_registration(
        &mut self,
        load: impl FnOnce() -> VmBuiltinRegistration,
    ) -> bool {
        if !self.builtins.is_empty()
            || !self.async_builtins.is_empty()
            || !self.capability_methods.is_empty()
        {
            return false;
        }
        let registration = load();
        self.builtins = registration.builtins;
        self.async_builtins = registration.async_builtins;
        self.capability_methods = registration.capability_methods;
        self.builtin_metadata = registration.builtin_metadata;
        self.builtins_by_id = registration.builtins_by_id;
        self.builtin_id_collisions = registration.builtin_id_collisions;
        if self.globals.is_empty() {
            self.globals = registration.globals;
        } else {
            Arc::make_mut(&mut self.globals).extend(
                registration
                    .globals
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone())),
            );
        }
        true
    }
}
