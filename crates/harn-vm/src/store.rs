//! Persistent key-value store backed by Harn's runtime state root.
//!
//! Provides `store_get`, `store_set`, `store_delete`, `store_list`,
//! `store_save`, and `store_clear` builtins. The store file is created
//! lazily on first mutation.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) struct StoreState {
    data: BTreeMap<String, serde_json::Value>,
    path: PathBuf,
    loaded: bool,
    /// Set when a branch opened this store: it lives in memory and never
    /// writes its file.
    overlay: bool,
}

impl StoreState {
    fn new(base_dir: &Path) -> Self {
        Self::at_path(crate::runtime_paths::store_path(base_dir))
    }

    fn at_state_root(state_root: &Path) -> Self {
        Self::at_path(state_root.join(crate::runtime_paths::STORE_FILE))
    }

    pub(crate) fn at_path(path: PathBuf) -> Self {
        Self {
            data: BTreeMap::new(),
            path,
            loaded: false,
            overlay: false,
        }
    }

    fn ensure_loaded(&mut self) -> Result<(), VmError> {
        if self.loaded {
            return Ok(());
        }
        let path = self.path.clone();
        let opened = crate::run_tape::store_seed(&path, || {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|contents| serde_json::from_str(&contents).ok())
        })?;
        self.loaded = true;
        self.overlay = opened.overlay;
        if let Some(serde_json::Value::Object(map)) = opened.contents {
            self.data.extend(map);
        }
        Ok(())
    }

    fn save(&self) -> Result<(), VmError> {
        if self.overlay {
            return Ok(());
        }
        let obj: serde_json::Map<String, serde_json::Value> = self
            .data
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let json = serde_json::to_string_pretty(&serde_json::Value::Object(obj))
            .map_err(|e| VmError::Runtime(format!("store save error: {e}")))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| VmError::Runtime(format!("store mkdir error: {e}")))?;
        }
        std::fs::write(&self.path, json)
            .map_err(|e| VmError::Runtime(format!("store write error: {e}")))
    }

    fn get(&mut self, key: &str) -> Result<Option<serde_json::Value>, VmError> {
        self.ensure_loaded()?;
        Ok(self.data.get(key).cloned())
    }

    pub(crate) fn set(&mut self, key: String, value: serde_json::Value) -> Result<(), VmError> {
        self.ensure_loaded()?;
        self.data.insert(key, value);
        self.save()
    }

    fn delete(&mut self, key: &str) -> Result<(), VmError> {
        self.ensure_loaded()?;
        self.data.remove(key);
        self.save()
    }

    fn list(&mut self) -> Result<Vec<String>, VmError> {
        self.ensure_loaded()?;
        Ok(self.data.keys().cloned().collect())
    }

    fn clear(&mut self) -> Result<(), VmError> {
        self.ensure_loaded()?;
        self.data.clear();
        self.save()
    }
}

use crate::json_to_vm_value;
use crate::value::vm_to_storage_json as vm_to_json;

/// Read `key`. A branch's store starts from what its recording opened, so a
/// replayed read never sees what the store gained since.
pub(crate) fn store_get(
    state: &parking_lot::Mutex<StoreState>,
    key: &str,
) -> Result<VmValue, VmError> {
    let value = state.lock().get(key)?;
    Ok(value.as_ref().map_or(VmValue::Nil, json_to_vm_value))
}

/// Register persistent key-value store builtins on a VM.
///
/// The store is backed by `<state-root>/store.json`, created lazily
/// on first mutation. In bridge mode, register these **before** bridge
/// builtins so the host can override them.
pub fn register_store_builtins(vm: &mut Vm, base_dir: &Path) {
    if let Err(error) = crate::event_log::install_lazy_default_for_base_dir(base_dir) {
        crate::events::log_warn("event_log.init", &error.to_string());
    }
    register_store_state(vm, StoreState::new(base_dir));
}

pub(crate) fn register_store_builtins_at_state_root(vm: &mut Vm, state_root: &Path) {
    register_store_state(vm, StoreState::at_state_root(state_root));
}

fn register_store_state(vm: &mut Vm, state: StoreState) {
    let state = Arc::new(parking_lot::Mutex::new(state));

    let s = Arc::clone(&state);
    vm.register_capability_method(
        harn_builtin_meta::CapabilityId::Runtime,
        "store_get",
        move |args, _out| {
            let key = args.first().map(|a| a.display()).unwrap_or_default();
            store_get(&s, &key)
        },
    );

    let s = Arc::clone(&state);
    vm.register_capability_method(
        harn_builtin_meta::CapabilityId::Runtime,
        "store_set",
        move |args, _out| {
            let key = args.first().map(|a| a.display()).unwrap_or_default();
            let value = args.get(1).unwrap_or(&VmValue::Nil);
            let json_val = vm_to_json(value)?;
            s.lock().set(key, json_val)?;
            Ok(VmValue::Nil)
        },
    );

    let s = Arc::clone(&state);
    vm.register_capability_method(
        harn_builtin_meta::CapabilityId::Runtime,
        "store_delete",
        move |args, _out| {
            let key = args.first().map(|a| a.display()).unwrap_or_default();
            s.lock().delete(&key)?;
            Ok(VmValue::Nil)
        },
    );

    let s = Arc::clone(&state);
    vm.register_capability_method(
        harn_builtin_meta::CapabilityId::Runtime,
        "store_list",
        move |_args, _out| {
            let keys = s.lock().list()?;
            Ok(VmValue::strings(keys))
        },
    );

    let s = Arc::clone(&state);
    vm.register_capability_method(
        harn_builtin_meta::CapabilityId::Runtime,
        "store_save",
        move |_args, _out| {
            // Load first: saving an unopened store would overwrite its file
            // with nothing, and a branch's open decides it is an overlay.
            let mut store = s.lock();
            store.ensure_loaded()?;
            store.save()?;
            Ok(VmValue::Nil)
        },
    );

    let s = Arc::clone(&state);
    vm.register_capability_method(
        harn_builtin_meta::CapabilityId::Runtime,
        "store_clear",
        move |_args, _out| {
            s.lock().clear()?;
            Ok(VmValue::Nil)
        },
    );

    vm.project_legacy_capability_globals();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::event_log::{
        active_event_log, install_memory_for_current_thread, reset_active_event_log,
    };

    #[test]
    fn register_store_builtins_preserves_existing_event_log() {
        reset_active_event_log();
        let existing = install_memory_for_current_thread(8);
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let mut vm = crate::new_vm();

        register_store_builtins(&mut vm, temp_dir.path());

        let active = active_event_log().expect("active event log");
        assert!(Arc::ptr_eq(&active, &existing));
        reset_active_event_log();
    }
}
