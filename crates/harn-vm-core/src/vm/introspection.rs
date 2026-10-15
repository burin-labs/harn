//! Read-only projections of the VM's installed builtin surface.

use super::{Vm, VmBuiltinMetadata};

impl Vm {
    pub fn has_builtin(&self, name: &str) -> bool {
        self.builtins.contains_key(name) || self.async_builtins.contains_key(name)
    }

    pub fn builtin_is_denied(&self, name: &str) -> bool {
        self.denied_builtins.contains(name)
    }

    pub fn runtime_context(&self) -> &super::RuntimeContext {
        &self.runtime_context
    }

    pub fn output_mut(&mut self) -> &mut String {
        &mut self.output
    }

    pub fn synchronization(&self) -> &std::sync::Arc<crate::synchronization::VmSyncRuntime> {
        &self.sync_runtime
    }

    pub fn shared_state(&self) -> &std::sync::Arc<crate::shared_state::VmSharedStateRuntime> {
        &self.shared_state_runtime
    }

    pub fn active_task_ids(&self) -> impl Iterator<Item = &str> {
        self.spawned_tasks.keys().map(String::as_str)
    }

    /// Return all registered builtin names (sync + async).
    pub fn builtin_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.builtins.keys().cloned().collect();
        names.extend(self.async_builtins.keys().cloned());
        names
    }

    /// Return every installed capability method as a `(capability, method)`
    /// pair, including those registered at runtime rather than declared
    /// through `#[harn_builtin]` exposure.
    pub fn capability_method_names(&self) -> Vec<(harn_builtin_meta::CapabilityId, String)> {
        self.capability_methods
            .iter()
            .flat_map(|(capability, methods)| {
                methods
                    .keys()
                    .map(move |method| (*capability, method.clone()))
            })
            .collect()
    }

    /// Return discoverable metadata for registered builtins.
    pub fn builtin_metadata(&self) -> Vec<VmBuiltinMetadata> {
        self.builtin_metadata.values().cloned().collect()
    }

    /// Return discoverable metadata for a registered builtin name.
    pub fn builtin_metadata_for(&self, name: &str) -> Option<&VmBuiltinMetadata> {
        self.builtin_metadata.get(name)
    }
}
