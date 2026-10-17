//! Read-only projections of the VM's installed builtin surface.

use super::{Vm, VmBuiltinMetadata};

impl Vm {
    pub fn source_dir(&self) -> Option<&std::path::Path> {
        self.source_dir.as_deref()
    }

    pub fn source_file(&self) -> Option<&str> {
        self.source_file.as_deref()
    }

    pub fn has_scope_deadline(&self) -> bool {
        !self.deadlines.is_empty()
    }

    pub fn has_builtin(&self, name: &str) -> bool {
        self.builtins.contains_key(name) || self.async_builtins.contains_key(name)
    }

    pub fn builtin_is_denied(&self, name: &str) -> bool {
        self.denied_builtins.contains(name)
    }

    pub fn runtime_context(&self) -> &super::RuntimeContext {
        &self.runtime_context
    }

    pub fn set_child_task_context(
        &mut self,
        task_id: impl Into<String>,
        name: impl Into<String>,
        group: Option<String>,
    ) {
        self.runtime_context = self.runtime_context.child_task(task_id, name, group);
    }

    pub fn cancellation_token(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        self.cancel_token.clone()
    }

    pub fn wait_for_graph(&self) -> &std::sync::Arc<crate::wait_for_graph::VmWaitForGraph> {
        &self.wait_for_graph
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

    pub fn inline_cache_entries(&self) -> impl Iterator<Item = &crate::InlineCacheEntry> {
        self.inline_cache_sets.iter().flatten()
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
