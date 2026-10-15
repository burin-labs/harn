//! Immutable stdlib module instances shared by one VM execution tree.
//!
//! A child VM inherits its parent's [`ModuleCache`] copy-on-write, so a module
//! the parent never imported is instantiated into the child's copy and dropped
//! when the child returns. Rust-to-Harn crossings (`stdlib::harn_entry`) run in
//! such transient children, so before this store every crossing into a cold
//! stdlib module replayed the module's whole import closure and initializer.
//!
//! The store only admits a module whose instance cannot be told apart from a
//! fresh one: it is embedded stdlib, its top-level bindings are all immutable
//! after initialization, and every module it imports is admitted too. Such an
//! instance holds no state a call can change, so reusing it matches a fresh
//! instantiation exactly. A module with a mutable top-level `let` (circuit
//! memos, registries) is never admitted and keeps today's per-VM instancing.
//!
//! Entries are keyed by synthetic module path plus everything that can change
//! the instance: the embedded source identity, the VM's module provenance,
//! and its sandbox denial set. Fresh VM roots start with an empty store, so
//! nothing crosses executions.

use std::collections::HashSet;

use super::*;
use crate::value::VmMutex;

/// One admitted instance and the identity it was instantiated under.
struct SharedModuleInstance {
    /// Address and length of the `'static` embedded source. Embedded text
    /// never changes in place, so a different source (a rebuilt stdlib or a
    /// test override) always has a different identity.
    source_identity: (usize, usize),
    provenance: ModuleProvenance,
    denied_builtins: Arc<HashSet<String>>,
    /// Closures hold only `Weak`s into their home module's registry and state,
    /// so a later child that hits this entry without importing the module's
    /// dependencies relies on the store to keep them alive. Admission requires
    /// every direct import's exact instance to be stored already, so the
    /// store strongly owns the whole transitive graph.
    loaded: Arc<LoadedModule>,
}

impl SharedModuleInstance {
    fn matches(
        &self,
        source_identity: (usize, usize),
        provenance: ModuleProvenance,
        denied: &Arc<HashSet<String>>,
    ) -> bool {
        self.source_identity == source_identity
            && self.provenance == provenance
            && (Arc::ptr_eq(&self.denied_builtins, denied) || *self.denied_builtins == **denied)
    }
}

/// Execution-tree store of admitted module instances. Cloned by `Arc` into
/// every child VM; fresh roots get their own.
#[derive(Default)]
pub(crate) struct SharedModuleInstanceStore {
    entries: VmMutex<BTreeMap<PathBuf, Vec<SharedModuleInstance>>>,
    /// Instantiations performed by the stdlib loader in this tree, by
    /// synthetic path. Tests use it to prove reuse and invalidation.
    #[cfg(test)]
    instantiations: VmMutex<BTreeMap<PathBuf, usize>>,
}

impl SharedModuleInstanceStore {
    #[cfg(test)]
    pub(crate) fn instantiation_count(&self, synthetic: &Path) -> usize {
        self.instantiations
            .lock()
            .get(synthetic)
            .copied()
            .unwrap_or(0)
    }
}

fn source_identity(source: &'static str) -> (usize, usize) {
    (source.as_ptr() as usize, source.len())
}

impl Vm {
    /// Look up an admitted instance of `synthetic` compiled from `source`.
    pub(super) fn shared_module_instance(
        &self,
        synthetic: &Path,
        source: &'static str,
    ) -> Option<Arc<LoadedModule>> {
        let identity = source_identity(source);
        let entries = self.shared_module_instances.entries.lock();
        entries
            .get(synthetic)?
            .iter()
            .find(|entry| entry.matches(identity, self.module_provenance, &self.denied_builtins))
            .map(|entry| Arc::clone(&entry.loaded))
    }

    /// Publish a freshly instantiated stdlib module when it is admissible:
    /// embedded stdlib, no mutable top-level binding after initialization,
    /// and every import bound to an instance this store already owns. Only
    /// admitted instances are stored, so the last rule makes admission
    /// transitive. An import still mid-load (a cycle) is not cached yet and
    /// so disqualifies the module.
    pub(super) fn publish_shared_module_instance(
        &self,
        synthetic: &Path,
        source: &'static str,
        artifact: &PreparedModuleArtifact,
        loaded: &Arc<LoadedModule>,
    ) {
        #[cfg(test)]
        {
            *self
                .shared_module_instances
                .instantiations
                .lock()
                .entry(synthetic.to_path_buf())
                .or_default() += 1;
        }
        if artifact.provenance != ModuleProvenance::EmbeddedStdlib
            || loaded._module_state.lock().has_mutable_binding()
        {
            return;
        }
        let identity = source_identity(source);
        let mut entries = self.shared_module_instances.entries.lock();
        let imports_are_stored = artifact.imports.iter().all(|import| {
            let Some(dependency) = self
                .cache_key_for_import(&import.path)
                .ok()
                .and_then(|key| self.module_cache.get(&key).map(|dep| (key, dep)))
            else {
                return false;
            };
            entries.get(&dependency.0).is_some_and(|slots| {
                slots
                    .iter()
                    .any(|entry| Arc::ptr_eq(&entry.loaded, dependency.1))
            })
        });
        if !imports_are_stored {
            return;
        }
        let slots = entries.entry(synthetic.to_path_buf()).or_default();
        // A concurrent sibling may have published first; either instance is
        // indistinguishable, so keep the earlier one.
        if slots
            .iter()
            .any(|entry| entry.matches(identity, self.module_provenance, &self.denied_builtins))
        {
            return;
        }
        slots.push(SharedModuleInstance {
            source_identity: identity,
            provenance: self.module_provenance,
            denied_builtins: Arc::clone(&self.denied_builtins),
            loaded: Arc::clone(loaded),
        });
    }
}
