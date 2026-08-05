//! Explicit persistent-state registration for isolated VM executions.

use std::path::Path;

use crate::Vm;

/// A caller-owned persistent-state root that bypasses ambient runtime paths.
#[derive(Clone, Copy, Debug)]
pub struct PersistentStateRoot<'a>(&'a Path);

impl<'a> PersistentStateRoot<'a> {
    #[must_use]
    pub fn new(path: &'a Path) -> Self {
        Self(path)
    }

    fn as_path(self) -> &'a Path {
        self.0
    }
}

/// Register store, metadata, and checkpoint builtins at an exact state root.
///
/// Unlike the individual runtime registrars, this function does not consult
/// `HARN_STATE_DIR`. Embedders use it when concurrent executions require
/// hermetic state without mutating process-global environment variables.
pub fn register_persistent_state_builtins_at_root(
    vm: &mut Vm,
    base_dir: &Path,
    state_root: PersistentStateRoot<'_>,
    pipeline_name: &str,
) {
    let state_root = state_root.as_path();
    crate::store::register_store_builtins_at_state_root(vm, state_root);
    crate::metadata::register_metadata_builtins_at_state_root(vm, base_dir, state_root);
    crate::checkpoint::register_checkpoint_builtins_at_state_root(vm, state_root, pipeline_name);
}
