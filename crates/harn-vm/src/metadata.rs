//! Project metadata store for `.burin/metadata/` sharded JSON files.
//!
//! Provides `metadata_get`, `metadata_set`, `metadata_save`, `metadata_stale`,
//! and `metadata_refresh_hashes` builtins. Compatible with the Swift
//! DirectoryMetadataStore format (sharded by package root).
//!
//! Resolution uses hierarchical inheritance: child directories inherit from
//! parent directories, with overrides at each level.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

type Namespace = String;
type FieldKey = String;

/// Per-directory metadata: namespaces -> keys -> JSON values.
#[derive(Clone, Default)]
struct DirectoryMetadata {
    namespaces: BTreeMap<Namespace, BTreeMap<FieldKey, serde_json::Value>>,
}

/// The full metadata store (all directories).
struct MetadataState {
    entries: BTreeMap<String, DirectoryMetadata>,
    base_dir: PathBuf,
    loaded: bool,
    dirty: bool,
}

impl MetadataState {
    fn new(base_dir: &Path) -> Self {
        Self {
            entries: BTreeMap::new(),
            base_dir: base_dir.to_path_buf(),
            loaded: false,
            dirty: false,
        }
    }

    fn metadata_dir(&self) -> PathBuf {
        self.base_dir.join(".burin").join("metadata")
    }

    fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        let meta_dir = self.metadata_dir();
        let entries = match std::fs::read_dir(&meta_dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    self.load_shard(&contents);
                }
            }
        }
    }

    fn load_shard(&mut self, contents: &str) {
        let parsed: serde_json::Value = match serde_json::from_str(contents) {
            Ok(v) => v,
            Err(_) => return,
        };
        let shard_entries = match parsed.get("entries").and_then(|e| e.as_object()) {
            Some(e) => e,
            None => return,
        };
        for (dir, meta_val) in shard_entries {
            let meta = parse_directory_metadata(meta_val);
            self.entries.insert(dir.clone(), meta);
        }
    }

    /// Resolve metadata for a directory with hierarchical inheritance.
    /// Walks from root (".") through each path component, merging at each level.
    fn resolve(&mut self, directory: &str) -> DirectoryMetadata {
        self.ensure_loaded();
        let mut result = DirectoryMetadata::default();

        // Start with root
        if let Some(root) = self.entries.get(".").or_else(|| self.entries.get("")) {
            merge_metadata(&mut result, root);
        }

        // Walk path components
        let components: Vec<&str> = directory
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .collect();
        let mut current = String::new();
        for component in components {
            if current.is_empty() {
                current = component.to_string();
            } else {
                current = format!("{current}/{component}");
            }
            if let Some(meta) = self.entries.get(&current) {
                merge_metadata(&mut result, meta);
            }
        }

        result
    }

    /// Get a specific namespace for a resolved directory.
    fn get_namespace(
        &mut self,
        directory: &str,
        namespace: &str,
    ) -> Option<BTreeMap<FieldKey, serde_json::Value>> {
        let resolved = self.resolve(directory);
        resolved.namespaces.get(namespace).cloned()
    }

    /// Set metadata for a directory + namespace.
    fn set_namespace(
        &mut self,
        directory: &str,
        namespace: &str,
        data: BTreeMap<FieldKey, serde_json::Value>,
    ) {
        self.ensure_loaded();
        let meta = self.entries.entry(directory.to_string()).or_default();
        let ns = meta.namespaces.entry(namespace.to_string()).or_default();
        for (k, v) in data {
            ns.insert(k, v);
        }
        self.dirty = true;
    }

    /// Save all metadata back to sharded JSON files.
    fn save(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let meta_dir = self.metadata_dir();
        std::fs::create_dir_all(&meta_dir).map_err(|e| format!("metadata mkdir: {e}"))?;

        // Shard by simple strategy: everything in one "root" shard for now.
        // This matches Swift behavior for single-package projects.
        let mut shard = serde_json::Map::new();
        for (dir, meta) in &self.entries {
            shard.insert(dir.clone(), serialize_directory_metadata(meta));
        }

        let store_obj = serde_json::json!({
            "version": 2,
            "generatedAt": chrono_now_iso(),
            "entries": serde_json::Value::Object(shard)
        });

        let json =
            serde_json::to_string_pretty(&store_obj).map_err(|e| format!("metadata json: {e}"))?;

        let shard_path = meta_dir.join("root.json");
        std::fs::write(&shard_path, json).map_err(|e| format!("metadata write: {e}"))?;
        self.dirty = false;
        Ok(())
    }
}

fn chrono_now_iso() -> String {
    // Simple ISO 8601 timestamp without chrono dependency
    let now = std::time::SystemTime::now();
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Approximate ISO format
    format!("{secs}")
}

fn merge_metadata(target: &mut DirectoryMetadata, source: &DirectoryMetadata) {
    for (ns, fields) in &source.namespaces {
        let target_ns = target.namespaces.entry(ns.clone()).or_default();
        for (k, v) in fields {
            target_ns.insert(k.clone(), v.clone());
        }
    }
}

fn parse_directory_metadata(val: &serde_json::Value) -> DirectoryMetadata {
    let mut meta = DirectoryMetadata::default();
    let obj = match val.as_object() {
        Some(o) => o,
        None => return meta,
    };
    // Parse "namespaces" key (the standard format)
    if let Some(ns_obj) = obj.get("namespaces").and_then(|n| n.as_object()) {
        for (ns_name, fields_val) in ns_obj {
            if let Some(fields) = fields_val.as_object() {
                let mut field_map = BTreeMap::new();
                for (k, v) in fields {
                    field_map.insert(k.clone(), v.clone());
                }
                meta.namespaces.insert(ns_name.clone(), field_map);
            }
        }
    }
    meta
}

fn serialize_directory_metadata(meta: &DirectoryMetadata) -> serde_json::Value {
    let mut ns_obj = serde_json::Map::new();
    for (ns_name, fields) in &meta.namespaces {
        let mut fields_obj = serde_json::Map::new();
        for (k, v) in fields {
            fields_obj.insert(k.clone(), v.clone());
        }
        ns_obj.insert(ns_name.clone(), serde_json::Value::Object(fields_obj));
    }
    serde_json::json!({ "namespaces": serde_json::Value::Object(ns_obj) })
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

/// Register metadata builtins on a VM.
///
/// In standalone mode, these operate directly on `.burin/metadata/` files.
/// In bridge mode, these are registered **before** bridge builtins so the
/// host can override them if needed (but typically the VM handles this natively).
pub fn register_metadata_builtins(vm: &mut Vm, base_dir: &Path) {
    let state = Rc::new(RefCell::new(MetadataState::new(base_dir)));

    // metadata_get(dir, namespace?) -> dict | nil
    let s = Rc::clone(&state);
    vm.register_builtin("metadata_get", move |args, _out| {
        let dir = args.first().map(|a| a.display()).unwrap_or_default();
        let namespace = args.get(1).and_then(|a| {
            if matches!(a, VmValue::Nil) {
                None
            } else {
                Some(a.display())
            }
        });

        let mut st = s.borrow_mut();
        if let Some(ns) = namespace {
            match st.get_namespace(&dir, &ns) {
                Some(fields) => {
                    let mut m = BTreeMap::new();
                    for (k, v) in fields {
                        m.insert(k, json_to_vm(&v));
                    }
                    Ok(VmValue::Dict(Rc::new(m)))
                }
                None => Ok(VmValue::Nil),
            }
        } else {
            // Return all namespaces flattened
            let resolved = st.resolve(&dir);
            let mut m = BTreeMap::new();
            for fields in resolved.namespaces.values() {
                for (k, v) in fields {
                    m.insert(k.clone(), json_to_vm(v));
                }
            }
            if m.is_empty() {
                Ok(VmValue::Nil)
            } else {
                Ok(VmValue::Dict(Rc::new(m)))
            }
        }
    });

    // metadata_set(dir, namespace, data_dict)
    let s = Rc::clone(&state);
    vm.register_builtin("metadata_set", move |args, _out| {
        let dir = args.first().map(|a| a.display()).unwrap_or_default();
        let namespace = args.get(1).map(|a| a.display()).unwrap_or_default();
        let data_val = args.get(2).unwrap_or(&VmValue::Nil);

        let mut data = BTreeMap::new();
        if let VmValue::Dict(dict) = data_val {
            for (k, v) in dict.iter() {
                data.insert(k.clone(), vm_to_json(v));
            }
        }

        if !data.is_empty() {
            s.borrow_mut().set_namespace(&dir, &namespace, data);
        }
        Ok(VmValue::Nil)
    });

    // metadata_save()
    let s = Rc::clone(&state);
    vm.register_builtin("metadata_save", move |_args, _out| {
        s.borrow_mut().save().map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    });

    // metadata_stale(project) -> {any_stale: bool, tier1: [], tier2: []}
    // In standalone mode, compare stored hashes against filesystem.
    // Returns empty (no stale) since we don't track hashes natively yet.
    vm.register_builtin("metadata_stale", |_args, _out| {
        let mut m = BTreeMap::new();
        m.insert("any_stale".to_string(), VmValue::Bool(false));
        m.insert("tier1".to_string(), VmValue::List(Rc::new(vec![])));
        m.insert("tier2".to_string(), VmValue::List(Rc::new(vec![])));
        Ok(VmValue::Dict(Rc::new(m)))
    });

    // metadata_refresh_hashes() -> nil
    vm.register_builtin("metadata_refresh_hashes", |_args, _out| Ok(VmValue::Nil));

    // compute_content_hash(dir) -> string
    // Simple hash of file list in directory for staleness tracking
    let base = base_dir.to_path_buf();
    vm.register_builtin("compute_content_hash", move |args, _out| {
        let dir = args.first().map(|a| a.display()).unwrap_or_default();
        let full_dir = base.join(&dir);
        let mut entries: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&full_dir) {
            for entry in rd.flatten() {
                if let Ok(meta) = entry.metadata() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    entries.push(format!("{}:{}", name, meta.len()));
                }
            }
        }
        entries.sort();
        // Simple hash: join and take a prefix
        let joined = entries.join("|");
        let hash = format!("{:x}", md5_simple(joined.as_bytes()));
        Ok(VmValue::String(Rc::from(hash.as_str())))
    });

    // invalidate_facts(dir) -> nil (no-op in standalone)
    vm.register_builtin("invalidate_facts", |_args, _out| Ok(VmValue::Nil));
}

/// Simple hash (not crypto-grade, just for staleness detection).
fn md5_simple(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
