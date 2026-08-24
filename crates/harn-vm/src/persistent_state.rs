//! Explicit persistent-state ownership for isolated VM executions.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::Vm;

thread_local! {
    static SCOPED_PERSISTENT_STATE_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

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

/// Restores the prior caller-owned persistent-state root on drop.
///
/// The guard is deliberately not `Send`: its scope is tied to the current
/// thread or `LocalSet`, just like the runtime paths it overrides.
#[derive(Debug)]
#[must_use = "retain this guard for the isolated VM execution"]
pub struct ScopedPersistentStateRoot {
    previous: Option<PathBuf>,
    _not_send: PhantomData<Rc<()>>,
}

/// Route every default persistent runtime path through one caller-owned root.
///
/// This is the execution-wide counterpart to
/// [`register_persistent_state_builtins_at_root`]. It covers durable consumers
/// such as the agent session journal that resolve their paths during execution
/// rather than when builtins are registered.
pub fn scope_persistent_state_root(root: PersistentStateRoot<'_>) -> ScopedPersistentStateRoot {
    let previous =
        SCOPED_PERSISTENT_STATE_ROOT.with(|slot| slot.replace(Some(root.as_path().to_path_buf())));
    ScopedPersistentStateRoot {
        previous,
        _not_send: PhantomData,
    }
}

impl Drop for ScopedPersistentStateRoot {
    fn drop(&mut self) {
        SCOPED_PERSISTENT_STATE_ROOT.with(|slot| {
            slot.replace(self.previous.take());
        });
    }
}

pub(crate) fn current_persistent_state_root() -> Option<PathBuf> {
    SCOPED_PERSISTENT_STATE_ROOT.with(|slot| slot.borrow().clone())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_persistent_state_root_restores_nested_owner() {
        let outer = Path::new("/isolated/outer/.harn");
        let inner = Path::new("/isolated/inner/.harn");
        assert_eq!(current_persistent_state_root(), None);
        let outer_guard = scope_persistent_state_root(PersistentStateRoot::new(outer));
        assert_eq!(current_persistent_state_root().as_deref(), Some(outer));
        {
            let _inner_guard = scope_persistent_state_root(PersistentStateRoot::new(inner));
            assert_eq!(current_persistent_state_root().as_deref(), Some(inner));
        }
        assert_eq!(current_persistent_state_root().as_deref(), Some(outer));
        drop(outer_guard);
        assert_eq!(current_persistent_state_root(), None);
    }
}
