//! Checkpoint system for resilient pipeline execution.
//!
//! Provides `checkpoint`, `checkpoint_get`, and `checkpoint_clear` builtins.
//! Checkpoints are persisted to `<state-root>/checkpoints/<pipeline>.json`
//! and survive pipeline crashes/timeouts. On resume, a pipeline can skip
//! already-processed items by checking `checkpoint_get`.
//!
//! The per-pipeline state is per-thread: `register_checkpoint_builtins`
//! installs a fresh [`CheckpointState`] into a thread-local cell so the
//! `#[harn_builtin]`-emitted handler fns can read/mutate it without
//! capturing closures (which would block the macro path). Each VM that
//! registers checkpoint builtins overrides the cell for its thread —
//! since the Harn VM is single-threaded per execution, this is the
//! intended scoping (each pipeline run installs its own state once at
//! VM setup, then the handlers see it for the duration of the run).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

struct CheckpointState {
    data: BTreeMap<String, serde_json::Value>,
    path: PathBuf,
    loaded: bool,
}

impl CheckpointState {
    fn new(base_dir: &Path, pipeline_name: &str) -> Self {
        Self {
            data: BTreeMap::new(),
            path: crate::runtime_paths::checkpoint_dir(base_dir)
                .join(format!("{pipeline_name}.json")),
            loaded: false,
        }
    }

    fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if let Ok(contents) = std::fs::read_to_string(&self.path) {
            if let Ok(serde_json::Value::Object(map)) =
                serde_json::from_str::<serde_json::Value>(&contents)
            {
                for (k, v) in map {
                    self.data.insert(k, v);
                }
            }
        }
    }

    fn save(&self) -> Result<(), String> {
        let obj: serde_json::Map<String, serde_json::Value> = self
            .data
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let json = serde_json::to_string_pretty(&serde_json::Value::Object(obj))
            .map_err(|e| format!("checkpoint save error: {e}"))?;
        crate::atomic_io::atomic_write(&self.path, json.as_bytes())
            .map_err(|e| format!("checkpoint write error: {e}"))?;
        Ok(())
    }

    fn get(&mut self, key: &str) -> VmValue {
        self.ensure_loaded();
        match self.data.get(key) {
            Some(v) => json_to_vm(v),
            None => VmValue::Nil,
        }
    }

    fn set(&mut self, key: String, value: serde_json::Value) -> Result<(), String> {
        self.ensure_loaded();
        self.data.insert(key, value);
        self.save()
    }

    fn clear(&mut self) -> Result<(), String> {
        self.data.clear();
        if self.path.exists() {
            std::fs::remove_file(&self.path).map_err(|e| format!("checkpoint clear error: {e}"))?;
        }
        Ok(())
    }

    fn list(&mut self) -> Vec<String> {
        self.ensure_loaded();
        self.data.keys().cloned().collect()
    }

    fn exists(&mut self, key: &str) -> bool {
        self.ensure_loaded();
        self.data.contains_key(key)
    }

    fn delete(&mut self, key: &str) -> Result<(), String> {
        self.ensure_loaded();
        self.data.remove(key);
        self.save()
    }
}

thread_local! {
    /// Active checkpoint state for the current thread's pipeline run.
    /// Set by `register_checkpoint_builtins`; read by the
    /// `#[harn_builtin]` handler fns. `None` before the first install
    /// (i.e. when the checkpoint builtins haven't been registered for
    /// this VM context) — in that case the handlers return a clear
    /// runtime error rather than a panic.
    static CHECKPOINT_STATE: RefCell<Option<CheckpointState>> = const { RefCell::new(None) };
}

fn with_state<R>(
    fn_name: &'static str,
    f: impl FnOnce(&mut CheckpointState) -> Result<R, VmError>,
) -> Result<R, VmError> {
    CHECKPOINT_STATE.with(|cell| {
        let mut guard = cell.borrow_mut();
        let state = guard.as_mut().ok_or_else(|| {
            VmError::Runtime(format!(
                "{fn_name}: checkpoint builtins not registered for this VM"
            ))
        })?;
        f(state)
    })
}

use crate::value::vm_to_storage_json as vm_to_json;

fn json_to_vm(jv: &serde_json::Value) -> VmValue {
    match jv {
        serde_json::Value::Null => VmValue::Nil,
        serde_json::Value::Bool(b) => VmValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                VmValue::Int(i)
            } else {
                VmValue::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => VmValue::String(arcstr::ArcStr::from(s.as_str())),
        serde_json::Value::Array(arr) => {
            VmValue::List(std::sync::Arc::new(arr.iter().map(json_to_vm).collect()))
        }
        serde_json::Value::Object(map) => {
            let mut m = BTreeMap::new();
            for (k, v) in map {
                m.insert(k.clone(), json_to_vm(v));
            }
            VmValue::dict(m)
        }
    }
}

/// Sanitize a pipeline name for use as a filename.
/// Rejects path traversal attempts and invalid characters.
fn sanitize_pipeline_name(name: &str) -> String {
    let base = std::path::Path::new(name)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("default");
    if base.is_empty() || base == "." || base == ".." {
        return "default".to_string();
    }
    base.to_string()
}

/// Register checkpoint builtins on a VM.
///
/// The pipeline name is used to namespace checkpoint files. If not provided,
/// defaults to "default". State is installed into a thread-local cell that
/// the `#[harn_builtin]`-emitted handlers below read; subsequent calls on
/// the same thread overwrite the state for that thread (the Harn VM
/// executes single-threaded per run).
pub fn register_checkpoint_builtins(vm: &mut Vm, base_dir: &Path, pipeline_name: &str) {
    let safe_name = sanitize_pipeline_name(pipeline_name);
    CHECKPOINT_STATE.with(|cell| {
        *cell.borrow_mut() = Some(CheckpointState::new(base_dir, &safe_name));
    });
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &CHECKPOINT_IMPL_DEF,
    &CHECKPOINT_GET_IMPL_DEF,
    &CHECKPOINT_CLEAR_IMPL_DEF,
    &CHECKPOINT_LIST_IMPL_DEF,
    &CHECKPOINT_EXISTS_IMPL_DEF,
    &CHECKPOINT_DELETE_IMPL_DEF,
];

#[harn_builtin(
    sig = "checkpoint(key: string, value: any) -> nil",
    category = "checkpoint",
    doc = "Persist a checkpoint key/value pair to durable storage immediately."
)]
fn checkpoint_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let key = args.first().map(|a| a.display()).unwrap_or_default();
    let value = args.get(1).unwrap_or(&VmValue::Nil);
    let json_val = vm_to_json(value);
    with_state("checkpoint", |state| {
        state.set(key, json_val).map_err(VmError::Runtime)
    })?;
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "checkpoint_get(key: string) -> any",
    category = "checkpoint",
    doc = "Read a persisted checkpoint value, or nil if the key is absent."
)]
fn checkpoint_get_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let key = args.first().map(|a| a.display()).unwrap_or_default();
    with_state("checkpoint_get", |state| Ok(state.get(&key)))
}

#[harn_builtin(
    sig = "checkpoint_clear() -> nil",
    category = "checkpoint",
    doc = "Clear every checkpoint for the active pipeline."
)]
fn checkpoint_clear_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    with_state("checkpoint_clear", |state| {
        state.clear().map_err(VmError::Runtime)
    })?;
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "checkpoint_list() -> list",
    category = "checkpoint",
    doc = "Return every checkpoint key for the active pipeline."
)]
fn checkpoint_list_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    with_state("checkpoint_list", |state| {
        let keys = state.list();
        Ok(VmValue::List(std::sync::Arc::new(
            keys.into_iter()
                .map(|k| VmValue::String(arcstr::ArcStr::from(k)))
                .collect(),
        )))
    })
}

#[harn_builtin(
    sig = "checkpoint_exists(key: string) -> bool",
    category = "checkpoint",
    doc = "Return true when the checkpoint key is present (even when its value is nil)."
)]
fn checkpoint_exists_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let key = args.first().map(|a| a.display()).unwrap_or_default();
    with_state("checkpoint_exists", |state| {
        Ok(VmValue::Bool(state.exists(&key)))
    })
}

#[harn_builtin(
    sig = "checkpoint_delete(key: string) -> nil",
    category = "checkpoint",
    doc = "Remove a single key from the checkpoint store."
)]
fn checkpoint_delete_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let key = args.first().map(|a| a.display()).unwrap_or_default();
    with_state("checkpoint_delete", |state| {
        state.delete(&key).map_err(VmError::Runtime)
    })?;
    Ok(VmValue::Nil)
}
