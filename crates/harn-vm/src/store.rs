//! Persistent key-value store backed by Harn's runtime state root.
//!
//! Provides `store_get`, `store_set`, `store_delete`, `store_list`,
//! `store_save`, and `store_clear` builtins. The store file is created
//! lazily on first mutation.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

struct StoreState {
    data: BTreeMap<String, serde_json::Value>,
    path: PathBuf,
    loaded: bool,
}

impl StoreState {
    fn new(base_dir: &Path) -> Self {
        Self {
            data: BTreeMap::new(),
            path: crate::runtime_paths::store_path(base_dir),
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
            .map_err(|e| format!("store save error: {e}"))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("store mkdir error: {e}"))?;
        }
        std::fs::write(&self.path, json).map_err(|e| format!("store write error: {e}"))?;
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

    fn delete(&mut self, key: &str) -> Result<(), String> {
        self.ensure_loaded();
        self.data.remove(key);
        self.save()
    }

    fn list(&mut self) -> Vec<String> {
        self.ensure_loaded();
        self.data.keys().cloned().collect()
    }

    fn clear(&mut self) -> Result<(), String> {
        self.ensure_loaded();
        self.data.clear();
        self.save()
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

/// Register persistent key-value store builtins on a VM.
///
/// The store is backed by `<state-root>/store.json`, created lazily
/// on first mutation. In bridge mode, register these **before** bridge
/// builtins so the host can override them.
pub fn register_store_builtins(vm: &mut Vm, base_dir: &Path) {
    if let Err(error) = crate::event_log::install_default_for_base_dir(base_dir) {
        crate::events::log_warn("event_log.init", &error.to_string());
    }
    let state = Rc::new(RefCell::new(StoreState::new(base_dir)));

    let s = Rc::clone(&state);
    vm.register_builtin("store_get", move |args, _out| {
        let key = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(s.borrow_mut().get(&key))
    });

    let s = Rc::clone(&state);
    vm.register_builtin("store_set", move |args, _out| {
        let key = args.first().map(|a| a.display()).unwrap_or_default();
        let value = args.get(1).unwrap_or(&VmValue::Nil);
        let json_val = vm_to_json(value);
        s.borrow_mut()
            .set(key, json_val)
            .map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    });

    let s = Rc::clone(&state);
    vm.register_builtin("store_delete", move |args, _out| {
        let key = args.first().map(|a| a.display()).unwrap_or_default();
        s.borrow_mut().delete(&key).map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    });

    let s = Rc::clone(&state);
    vm.register_builtin("store_list", move |_args, _out| {
        let keys = s.borrow_mut().list();
        Ok(VmValue::List(Rc::new(
            keys.into_iter()
                .map(|k| VmValue::String(Rc::from(k)))
                .collect(),
        )))
    });

    let s = Rc::clone(&state);
    vm.register_builtin("store_save", move |_args, _out| {
        s.borrow().save().map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    });

    let s = Rc::clone(&state);
    vm.register_builtin("store_clear", move |_args, _out| {
        s.borrow_mut().clear().map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    });
}
