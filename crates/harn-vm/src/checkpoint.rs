//! Checkpoint system for resilient pipeline execution.
//!
//! Provides `checkpoint`, `checkpoint_get`, and `checkpoint_clear` builtins.
//! Checkpoints are persisted to `<base_dir>/.harn/checkpoints/<pipeline>.json`
//! and survive pipeline crashes/timeouts. On resume, a pipeline can skip
//! already-processed items by checking `checkpoint_get`.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

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
            path: base_dir
                .join(".harn")
                .join("checkpoints")
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
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("checkpoint mkdir error: {e}"))?;
        }
        std::fs::write(&self.path, json)
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
            std::fs::remove_file(&self.path)
                .map_err(|e| format!("checkpoint clear error: {e}"))?;
        }
        Ok(())
    }

    fn list(&mut self) -> Vec<String> {
        self.ensure_loaded();
        self.data.keys().cloned().collect()
    }
}

fn vm_to_json(val: &VmValue) -> serde_json::Value {
    match val {
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::Int(n) => serde_json::json!(*n),
        VmValue::Float(n) => serde_json::json!(*n),
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(items) => serde_json::Value::Array(items.iter().map(vm_to_json).collect()),
        VmValue::Dict(map) => {
            let obj: serde_json::Map<String, serde_json::Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), vm_to_json(v)))
                .collect();
            serde_json::Value::Object(obj)
        }
        _ => serde_json::Value::Null,
    }
}

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
        serde_json::Value::String(s) => VmValue::String(Rc::from(s.as_str())),
        serde_json::Value::Array(arr) => {
            VmValue::List(Rc::new(arr.iter().map(json_to_vm).collect()))
        }
        serde_json::Value::Object(map) => {
            let mut m = BTreeMap::new();
            for (k, v) in map {
                m.insert(k.clone(), json_to_vm(v));
            }
            VmValue::Dict(Rc::new(m))
        }
    }
}

/// Sanitize a pipeline name for use as a filename.
/// Rejects path traversal attempts and invalid characters.
fn sanitize_pipeline_name(name: &str) -> String {
    // Use only the filename component, stripping any directory parts
    let base = std::path::Path::new(name)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("default");
    // Reject empty or dot-only names
    if base.is_empty() || base == "." || base == ".." {
        return "default".to_string();
    }
    base.to_string()
}

/// Register checkpoint builtins on a VM.
///
/// The pipeline name is used to namespace checkpoint files. If not provided,
/// defaults to "default".
pub fn register_checkpoint_builtins(vm: &mut Vm, base_dir: &Path, pipeline_name: &str) {
    let safe_name = sanitize_pipeline_name(pipeline_name);
    let state = Rc::new(RefCell::new(CheckpointState::new(base_dir, &safe_name)));

    // checkpoint(key, value) — persist a checkpoint immediately
    let s = Rc::clone(&state);
    vm.register_builtin("checkpoint", move |args, _out| {
        let key = args.first().map(|a| a.display()).unwrap_or_default();
        let value = args.get(1).unwrap_or(&VmValue::Nil);
        let json_val = vm_to_json(value);
        s.borrow_mut()
            .set(key, json_val)
            .map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    });

    // checkpoint_get(key) -> value | nil
    let s = Rc::clone(&state);
    vm.register_builtin("checkpoint_get", move |args, _out| {
        let key = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(s.borrow_mut().get(&key))
    });

    // checkpoint_clear() — clear all checkpoints for this pipeline
    let s = Rc::clone(&state);
    vm.register_builtin("checkpoint_clear", move |_args, _out| {
        s.borrow_mut().clear().map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    });

    // checkpoint_list() -> [key1, key2, ...]
    let s = Rc::clone(&state);
    vm.register_builtin("checkpoint_list", move |_args, _out| {
        let keys = s.borrow_mut().list();
        Ok(VmValue::List(Rc::new(
            keys.into_iter()
                .map(|k| VmValue::String(Rc::from(k)))
                .collect(),
        )))
    });
}
