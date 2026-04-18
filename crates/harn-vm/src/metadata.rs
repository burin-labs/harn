//! Project metadata store for Harn's runtime state root.
//!
//! Provides `metadata_get`, `metadata_set`, `metadata_save`, `metadata_stale`,
//! and `metadata_refresh_hashes` builtins. Stores sharded JSON files by
//! package root.
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
const LEGACY_SHARD_NAME: &str = "root.json";
const NAMESPACE_ENTRIES_FILE: &str = "entries.json";

/// Per-directory metadata: namespaces -> keys -> JSON values.
#[derive(Clone, Default)]
struct DirectoryMetadata {
    namespaces: BTreeMap<Namespace, BTreeMap<FieldKey, serde_json::Value>>,
}

trait MetadataBackend {
    fn backend_name(&self) -> &'static str;
    fn load(&self, root: &Path) -> Result<BTreeMap<String, DirectoryMetadata>, String>;
    fn save(
        &self,
        root: &Path,
        entries: &BTreeMap<String, DirectoryMetadata>,
    ) -> Result<(), String>;
}

#[derive(Default)]
struct FilesystemMetadataBackend;

impl FilesystemMetadataBackend {
    fn new() -> Self {
        Self
    }
}

/// The full metadata store (all directories).
struct MetadataState {
    entries: BTreeMap<String, DirectoryMetadata>,
    base_dir: PathBuf,
    backend: Box<dyn MetadataBackend>,
    loaded: bool,
    dirty: bool,
}

impl MetadataState {
    fn new(base_dir: &Path) -> Self {
        Self {
            entries: BTreeMap::new(),
            base_dir: base_dir.to_path_buf(),
            backend: Box::new(FilesystemMetadataBackend::new()),
            loaded: false,
            dirty: false,
        }
    }

    fn metadata_dir(&self) -> PathBuf {
        crate::runtime_paths::metadata_dir(&self.base_dir)
    }

    fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if let Ok(entries) = self.backend.load(&self.metadata_dir()) {
            self.entries = entries;
        }
    }

    /// Resolve metadata for a directory with hierarchical inheritance.
    /// Walks from root (".") through each path component, merging at each level.
    fn resolve(&mut self, directory: &str) -> DirectoryMetadata {
        self.ensure_loaded();
        let mut result = DirectoryMetadata::default();

        if let Some(root) = self.entries.get(".").or_else(|| self.entries.get("")) {
            merge_metadata(&mut result, root);
        }

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

    fn local_directory(&mut self, directory: &str) -> DirectoryMetadata {
        self.ensure_loaded();
        self.entries.get(directory).cloned().unwrap_or_default()
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
        self.backend.save(&meta_dir, &self.entries)?;
        self.dirty = false;
        Ok(())
    }
}

impl MetadataBackend for FilesystemMetadataBackend {
    fn backend_name(&self) -> &'static str {
        "filesystem"
    }

    fn load(&self, root: &Path) -> Result<BTreeMap<String, DirectoryMetadata>, String> {
        let mut entries = BTreeMap::new();
        let legacy_path = root.join(LEGACY_SHARD_NAME);
        if let Ok(contents) = std::fs::read_to_string(&legacy_path) {
            entries = parse_legacy_entries(&contents);
        }

        let namespace_dirs = match std::fs::read_dir(root) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
            Err(error) => return Err(format!("metadata load: {error}")),
        };

        let mut dirs = namespace_dirs
            .flatten()
            .filter(|entry| entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false))
            .collect::<Vec<_>>();
        dirs.sort_by_key(|entry| entry.file_name());

        for dir in dirs {
            let shard_path = dir.path().join(NAMESPACE_ENTRIES_FILE);
            let Ok(contents) = std::fs::read_to_string(&shard_path) else {
                continue;
            };
            merge_namespace_entries(&mut entries, &contents);
        }

        Ok(entries)
    }

    fn save(
        &self,
        root: &Path,
        entries: &BTreeMap<String, DirectoryMetadata>,
    ) -> Result<(), String> {
        std::fs::create_dir_all(root).map_err(|error| format!("metadata mkdir: {error}"))?;

        let mut namespaces: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
            BTreeMap::new();
        for (dir, meta) in entries {
            for (namespace, fields) in &meta.namespaces {
                namespaces
                    .entry(namespace.clone())
                    .or_default()
                    .insert(dir.clone(), serialize_namespace_fields(fields));
            }
        }

        for (namespace, shard_entries) in namespaces {
            let namespace_dir = root.join(namespace_path_component(&namespace));
            std::fs::create_dir_all(&namespace_dir)
                .map_err(|error| format!("metadata mkdir: {error}"))?;
            let shard = serde_json::json!({
                "version": 1,
                "namespace": namespace,
                "backend": self.backend_name(),
                "generatedAt": chrono_now_iso(),
                "entries": serde_json::Value::Object(shard_entries),
            });
            let json = serde_json::to_string_pretty(&shard)
                .map_err(|error| format!("metadata json: {error}"))?;
            std::fs::write(namespace_dir.join(NAMESPACE_ENTRIES_FILE), json)
                .map_err(|error| format!("metadata write: {error}"))?;
        }

        Ok(())
    }
}

/// ISO 8601 timestamp (e.g. `2026-03-29T14:00:00Z`) without a chrono dependency.
fn chrono_now_iso() -> String {
    let now = std::time::SystemTime::now();
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;
    let mut y = 1970i64;
    let mut remaining = days as i64;
    loop {
        let days_in_year: i64 = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days: [i64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0usize;
    for days in &month_days {
        if remaining < *days {
            break;
        }
        remaining -= *days;
        m += 1;
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m + 1,
        remaining + 1,
        hours,
        minutes,
        seconds
    )
}

fn merge_metadata(target: &mut DirectoryMetadata, source: &DirectoryMetadata) {
    for (ns, fields) in &source.namespaces {
        let target_ns = target.namespaces.entry(ns.clone()).or_default();
        for (k, v) in fields {
            target_ns.insert(k.clone(), v.clone());
        }
    }
}

fn parse_namespace_fields(val: &serde_json::Value) -> BTreeMap<FieldKey, serde_json::Value> {
    let mut fields = BTreeMap::new();
    let Some(obj) = val.as_object() else {
        return fields;
    };
    for (key, value) in obj {
        fields.insert(key.clone(), value.clone());
    }
    fields
}

fn serialize_namespace_fields(fields: &BTreeMap<FieldKey, serde_json::Value>) -> serde_json::Value {
    let mut fields_obj = serde_json::Map::new();
    for (k, v) in fields {
        fields_obj.insert(k.clone(), v.clone());
    }
    serde_json::Value::Object(fields_obj)
}

fn parse_directory_metadata(val: &serde_json::Value) -> DirectoryMetadata {
    let mut meta = DirectoryMetadata::default();
    let obj = match val.as_object() {
        Some(o) => o,
        None => return meta,
    };
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

fn parse_legacy_entries(contents: &str) -> BTreeMap<String, DirectoryMetadata> {
    let mut entries = BTreeMap::new();
    let parsed: serde_json::Value = match serde_json::from_str(contents) {
        Ok(v) => v,
        Err(_) => return entries,
    };
    let Some(shard_entries) = parsed.get("entries").and_then(|e| e.as_object()) else {
        return entries;
    };
    for (dir, meta_val) in shard_entries {
        entries.insert(dir.clone(), parse_directory_metadata(meta_val));
    }
    entries
}

fn merge_namespace_entries(entries: &mut BTreeMap<String, DirectoryMetadata>, contents: &str) {
    let parsed: serde_json::Value = match serde_json::from_str(contents) {
        Ok(v) => v,
        Err(_) => return,
    };
    let Some(namespace) = parsed.get("namespace").and_then(|value| value.as_str()) else {
        return;
    };
    let Some(shard_entries) = parsed.get("entries").and_then(|value| value.as_object()) else {
        return;
    };
    for (dir, fields_val) in shard_entries {
        let directory = entries.entry(dir.clone()).or_default();
        directory
            .namespaces
            .insert(namespace.to_string(), parse_namespace_fields(fields_val));
    }
}

fn namespace_path_component(namespace: &str) -> String {
    let mut result = String::new();
    for ch in namespace.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => result.push(ch),
            _ => result.push_str(&format!("_{:02X}", ch as u32)),
        }
    }
    if result.is_empty() || result == "." || result == ".." {
        "_".to_string()
    } else {
        result
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

fn namespace_fields_to_vm(fields: &BTreeMap<FieldKey, serde_json::Value>) -> VmValue {
    let mut map = BTreeMap::new();
    for (k, v) in fields {
        map.insert(k.clone(), json_to_vm(v));
    }
    VmValue::Dict(Rc::new(map))
}

fn directory_metadata_to_vm(meta: &DirectoryMetadata) -> VmValue {
    let mut namespaces = BTreeMap::new();
    for (ns, fields) in &meta.namespaces {
        namespaces.insert(ns.clone(), namespace_fields_to_vm(fields));
    }
    VmValue::Dict(Rc::new(namespaces))
}

fn normalize_directory_key(dir: &str) -> String {
    if dir.trim().is_empty() || dir == "." {
        ".".to_string()
    } else {
        dir.to_string()
    }
}

#[derive(Clone)]
struct ScanOptions {
    pattern: Option<String>,
    max_depth: usize,
    include_hidden: bool,
    include_dirs: bool,
    include_files: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            pattern: None,
            max_depth: 5,
            include_hidden: false,
            include_dirs: true,
            include_files: true,
        }
    }
}

fn bool_arg(map: &BTreeMap<String, VmValue>, key: &str, default: bool) -> bool {
    match map.get(key) {
        Some(VmValue::Bool(value)) => *value,
        _ => default,
    }
}

fn usize_arg(map: &BTreeMap<String, VmValue>, key: &str, default: usize) -> usize {
    match map.get(key) {
        Some(VmValue::Int(value)) if *value >= 0 => *value as usize,
        _ => default,
    }
}

fn parse_scan_options(
    pattern_or_options: Option<&VmValue>,
    explicit_options: Option<&VmValue>,
) -> ScanOptions {
    let mut options = ScanOptions::default();
    if let Some(VmValue::String(pattern)) = pattern_or_options {
        options.pattern = Some(pattern.to_string());
    } else if let Some(VmValue::Dict(dict)) = pattern_or_options {
        apply_scan_options_dict(&mut options, dict);
    }
    if let Some(VmValue::Dict(dict)) = explicit_options {
        apply_scan_options_dict(&mut options, dict);
    }
    options
}

fn apply_scan_options_dict(options: &mut ScanOptions, dict: &BTreeMap<String, VmValue>) {
    if let Some(pattern) = dict.get("pattern").map(|value| value.display()) {
        if !pattern.is_empty() {
            options.pattern = Some(pattern);
        }
    }
    options.max_depth = usize_arg(dict, "max_depth", options.max_depth);
    options.include_hidden = bool_arg(dict, "include_hidden", options.include_hidden);
    options.include_dirs = bool_arg(dict, "include_dirs", options.include_dirs);
    options.include_files = bool_arg(dict, "include_files", options.include_files);
}

fn resolve_scan_root(rel_dir: &str) -> PathBuf {
    let candidate = PathBuf::from(rel_dir);
    if candidate.is_absolute() {
        return candidate;
    }
    crate::stdlib::process::resolve_source_relative_path(rel_dir)
}

/// Register metadata builtins on a VM.
///
/// In standalone mode, these operate directly on the resolved Harn metadata
/// state root.
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
            // Return all namespaces flattened.
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

    // metadata_resolve(dir, namespace?) -> dict | nil
    let s = Rc::clone(&state);
    vm.register_builtin("metadata_resolve", move |args, _out| {
        let dir = args.first().map(|a| a.display()).unwrap_or_default();
        let namespace = args.get(1).and_then(|a| {
            if matches!(a, VmValue::Nil) {
                None
            } else {
                Some(a.display())
            }
        });
        let mut st = s.borrow_mut();
        let resolved = st.resolve(&dir);
        if let Some(ns) = namespace {
            match resolved.namespaces.get(&ns) {
                Some(fields) => Ok(namespace_fields_to_vm(fields)),
                None => Ok(VmValue::Nil),
            }
        } else if resolved.namespaces.is_empty() {
            Ok(VmValue::Nil)
        } else {
            Ok(directory_metadata_to_vm(&resolved))
        }
    });

    // metadata_entries(namespace?) -> list
    let s = Rc::clone(&state);
    vm.register_builtin("metadata_entries", move |args, _out| {
        let namespace = args.first().and_then(|a| {
            if matches!(a, VmValue::Nil) {
                None
            } else {
                Some(a.display())
            }
        });
        let mut st = s.borrow_mut();
        st.ensure_loaded();
        let directories: Vec<String> = st.entries.keys().cloned().collect();
        let mut items = Vec::new();
        for dir in directories {
            let local = st.local_directory(&dir);
            let resolved = st.resolve(&dir);
            let mut item = BTreeMap::new();
            item.insert(
                "dir".to_string(),
                VmValue::String(Rc::from(normalize_directory_key(&dir))),
            );
            match &namespace {
                Some(ns) => {
                    item.insert(
                        "local".to_string(),
                        local
                            .namespaces
                            .get(ns)
                            .map(namespace_fields_to_vm)
                            .unwrap_or(VmValue::Nil),
                    );
                    item.insert(
                        "resolved".to_string(),
                        resolved
                            .namespaces
                            .get(ns)
                            .map(namespace_fields_to_vm)
                            .unwrap_or(VmValue::Nil),
                    );
                }
                None => {
                    item.insert("local".to_string(), directory_metadata_to_vm(&local));
                    item.insert("resolved".to_string(), directory_metadata_to_vm(&resolved));
                }
            }
            items.push(VmValue::Dict(Rc::new(item)));
        }
        Ok(VmValue::List(Rc::new(items)))
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

    // metadata_stale(project) -> {any_stale: bool, tier1: [dirs], tier2: [dirs]}
    // Compare stored structureHash/contentHash against current filesystem state.
    let s = Rc::clone(&state);
    let base2 = base_dir.to_path_buf();
    vm.register_builtin("metadata_stale", move |_args, _out| {
        s.borrow_mut().ensure_loaded();
        let state = s.borrow();
        let mut tier1_stale: Vec<VmValue> = Vec::new();
        let mut tier2_stale: Vec<VmValue> = Vec::new();

        for (dir, meta) in &state.entries {
            let full_dir = if dir.is_empty() {
                base2.clone()
            } else {
                base2.join(dir)
            };
            // Tier 1: structureHash — file list + sizes.
            if let Some(stored_hash) = meta
                .namespaces
                .get("classification")
                .and_then(|ns| ns.get("structureHash"))
                .and_then(|v| v.as_str())
            {
                let current_hash = compute_structure_hash(&full_dir);
                if current_hash != stored_hash {
                    tier1_stale.push(VmValue::String(Rc::from(dir.as_str())));
                    // Structure changed — skip the tier 2 content check.
                    continue;
                }
            }
            // Tier 2: contentHash — file content digest.
            if let Some(stored_hash) = meta
                .namespaces
                .get("classification")
                .and_then(|ns| ns.get("contentHash"))
                .and_then(|v| v.as_str())
            {
                let current_hash = compute_content_hash_for_dir(&full_dir);
                if current_hash != stored_hash {
                    tier2_stale.push(VmValue::String(Rc::from(dir.as_str())));
                }
            }
        }

        let any_stale = !tier1_stale.is_empty() || !tier2_stale.is_empty();
        let mut m = BTreeMap::new();
        m.insert("any_stale".to_string(), VmValue::Bool(any_stale));
        m.insert("tier1".to_string(), VmValue::List(Rc::new(tier1_stale)));
        m.insert("tier2".to_string(), VmValue::List(Rc::new(tier2_stale)));
        Ok(VmValue::Dict(Rc::new(m)))
    });

    // metadata_refresh_hashes(project) -> nil
    // Recompute and store structureHash for all directories.
    let s = Rc::clone(&state);
    let base3 = base_dir.to_path_buf();
    vm.register_builtin("metadata_refresh_hashes", move |_args, _out| {
        let mut state = s.borrow_mut();
        state.ensure_loaded();
        let dirs: Vec<String> = state.entries.keys().cloned().collect();
        for dir in dirs {
            let full_dir = if dir.is_empty() {
                base3.clone()
            } else {
                base3.join(&dir)
            };
            let hash = compute_structure_hash(&full_dir);
            let entry = state.entries.entry(dir).or_default();
            let ns = entry
                .namespaces
                .entry("classification".to_string())
                .or_default();
            ns.insert("structureHash".to_string(), serde_json::Value::String(hash));
        }
        state.dirty = true;
        Ok(VmValue::Nil)
    });

    // metadata_status(namespace?) -> dict
    let s = Rc::clone(&state);
    let base4 = base_dir.to_path_buf();
    vm.register_builtin("metadata_status", move |args, _out| {
        let namespace = args.first().and_then(|a| {
            if matches!(a, VmValue::Nil) {
                None
            } else {
                Some(a.display())
            }
        });
        s.borrow_mut().ensure_loaded();
        let state = s.borrow();
        let mut namespaces = BTreeMap::new();
        let mut directories = Vec::new();
        let mut missing_structure_hash = Vec::new();
        let mut missing_content_hash = Vec::new();
        for (dir, meta) in &state.entries {
            directories.push(VmValue::String(Rc::from(normalize_directory_key(dir))));
            for ns in meta.namespaces.keys() {
                namespaces.insert(ns.clone(), VmValue::Bool(true));
            }
            let full_dir = if dir.is_empty() {
                base4.clone()
            } else {
                base4.join(dir)
            };
            let relevant = namespace
                .as_ref()
                .and_then(|name| meta.namespaces.get(name))
                .or_else(|| meta.namespaces.get("classification"));
            if let Some(fields) = relevant {
                if !fields.contains_key("structureHash") && full_dir.exists() {
                    missing_structure_hash
                        .push(VmValue::String(Rc::from(normalize_directory_key(dir))));
                }
                if !fields.contains_key("contentHash") && full_dir.exists() {
                    missing_content_hash
                        .push(VmValue::String(Rc::from(normalize_directory_key(dir))));
                }
            }
        }
        let stale = metadata_stale_value(&state, &base4);
        let mut result = BTreeMap::new();
        result.insert(
            "directory_count".to_string(),
            VmValue::Int(state.entries.len() as i64),
        );
        result.insert(
            "namespace_count".to_string(),
            VmValue::Int(namespaces.len() as i64),
        );
        result.insert(
            "namespaces".to_string(),
            VmValue::List(Rc::new(
                namespaces
                    .keys()
                    .cloned()
                    .map(|name| VmValue::String(Rc::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "directories".to_string(),
            VmValue::List(Rc::new(directories)),
        );
        result.insert(
            "missing_structure_hash".to_string(),
            VmValue::List(Rc::new(missing_structure_hash)),
        );
        result.insert(
            "missing_content_hash".to_string(),
            VmValue::List(Rc::new(missing_content_hash)),
        );
        result.insert("stale".to_string(), stale);
        Ok(VmValue::Dict(Rc::new(result)))
    });

    // compute_content_hash(dir) -> string of file list + sizes + mtimes for staleness tracking.
    let base = base_dir.to_path_buf();
    vm.register_builtin("compute_content_hash", move |args, _out| {
        let dir = args.first().map(|a| a.display()).unwrap_or_default();
        let full_dir = if dir.is_empty() {
            base.clone()
        } else {
            base.join(&dir)
        };
        let hash = compute_content_hash_for_dir(&full_dir);
        Ok(VmValue::String(Rc::from(hash)))
    });

    // invalidate_facts is a no-op: facts live in the metadata namespace.
    vm.register_builtin("invalidate_facts", |_args, _out| Ok(VmValue::Nil));

    register_scan_builtins(vm);
}

/// Compute structure hash for a directory (file names + sizes).
fn compute_structure_hash(dir: &Path) -> String {
    let mut entries: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            if let Ok(meta) = entry.metadata() {
                let name = entry.file_name().to_string_lossy().into_owned();
                entries.push(format!("{}:{}", name, meta.len()));
            }
        }
    }
    entries.sort();
    let joined = entries.join("|");
    format!("{:x}", fnv_hash(joined.as_bytes()))
}

/// Compute content hash for a directory (file names + sizes + mtimes).
fn compute_content_hash_for_dir(dir: &Path) -> String {
    let mut entries: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            if let Ok(meta) = entry.metadata() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                entries.push(format!("{}:{}:{}", name, meta.len(), mtime));
            }
        }
    }
    entries.sort();
    let joined = entries.join("|");
    format!("{:x}", fnv_hash(joined.as_bytes()))
}

/// FNV-1a hash (not crypto-grade, just for staleness detection).
fn fnv_hash(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Register scan_directory builtin: native Rust file enumeration.
pub fn register_scan_builtins(vm: &mut Vm) {
    // scan_directory(path?, pattern?) -> [{path, size, modified, is_dir}, ...]
    vm.register_builtin("scan_directory", move |args, _out| {
        let rel_dir = args.first().map(|a| a.display()).unwrap_or_default();
        let options = parse_scan_options(args.get(1), args.get(2));
        let scan_base = resolve_scan_root(".");
        let full_dir = if rel_dir.is_empty() {
            scan_base.clone()
        } else {
            scan_base.join(&rel_dir)
        };
        let mut results: Vec<VmValue> = Vec::new();
        scan_dir_recursive(&full_dir, &scan_base, &options, &mut results, 0);
        Ok(VmValue::List(Rc::new(results)))
    });
}

fn metadata_stale_value(state: &MetadataState, base_dir: &Path) -> VmValue {
    let mut tier1_stale: Vec<VmValue> = Vec::new();
    let mut tier2_stale: Vec<VmValue> = Vec::new();
    for (dir, meta) in &state.entries {
        let full_dir = if dir.is_empty() {
            base_dir.to_path_buf()
        } else {
            base_dir.join(dir)
        };
        if let Some(stored_hash) = meta
            .namespaces
            .get("classification")
            .and_then(|ns| ns.get("structureHash"))
            .and_then(|v| v.as_str())
        {
            let current_hash = compute_structure_hash(&full_dir);
            if current_hash != stored_hash {
                tier1_stale.push(VmValue::String(Rc::from(normalize_directory_key(dir))));
                continue;
            }
        }
        if let Some(stored_hash) = meta
            .namespaces
            .get("classification")
            .and_then(|ns| ns.get("contentHash"))
            .and_then(|v| v.as_str())
        {
            let current_hash = compute_content_hash_for_dir(&full_dir);
            if current_hash != stored_hash {
                tier2_stale.push(VmValue::String(Rc::from(normalize_directory_key(dir))));
            }
        }
    }
    let any_stale = !tier1_stale.is_empty() || !tier2_stale.is_empty();
    let mut m = BTreeMap::new();
    m.insert("any_stale".to_string(), VmValue::Bool(any_stale));
    m.insert("tier1".to_string(), VmValue::List(Rc::new(tier1_stale)));
    m.insert("tier2".to_string(), VmValue::List(Rc::new(tier2_stale)));
    VmValue::Dict(Rc::new(m))
}

fn scan_dir_recursive(
    dir: &Path,
    base: &Path,
    options: &ScanOptions,
    results: &mut Vec<VmValue>,
    depth: usize,
) {
    if depth > options.max_depth {
        return;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !options.include_hidden && name.starts_with('.') {
            continue;
        }
        let rel_path = entry
            .path()
            .strip_prefix(base)
            .unwrap_or(entry.path().as_path())
            .to_string_lossy()
            .into_owned();
        if let Some(pat) = &options.pattern {
            if !glob_match(pat, &rel_path) {
                if meta.is_dir() {
                    scan_dir_recursive(&entry.path(), base, options, results, depth + 1);
                }
                continue;
            }
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let mut m = BTreeMap::new();
        m.insert("path".to_string(), VmValue::String(Rc::from(rel_path)));
        m.insert("size".to_string(), VmValue::Int(meta.len() as i64));
        m.insert("modified".to_string(), VmValue::Int(mtime));
        m.insert("is_dir".to_string(), VmValue::Bool(meta.is_dir()));
        if (meta.is_dir() && options.include_dirs) || (!meta.is_dir() && options.include_files) {
            results.push(VmValue::Dict(Rc::new(m)));
        }
        if meta.is_dir() {
            scan_dir_recursive(&entry.path(), base, options, results, depth + 1);
        }
    }
}

/// Simple glob matching (supports * and ** patterns).
fn glob_match(pattern: &str, path: &str) -> bool {
    if pattern.contains("**") {
        let parts: Vec<&str> = pattern.split("**").collect();
        if parts.len() == 2 {
            let prefix = parts[0].trim_end_matches('/');
            let suffix = parts[1].trim_start_matches('/');
            let prefix_ok = prefix.is_empty() || path.starts_with(prefix);
            let suffix_ok = suffix.is_empty() || path.ends_with(suffix);
            return prefix_ok && suffix_ok;
        }
    }
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            return path.starts_with(parts[0]) && path.ends_with(parts[1]);
        }
    }
    path.contains(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("harn-metadata-{name}-{unique}"))
    }

    #[test]
    fn metadata_resolve_preserves_namespace_structure() {
        let base = temp_path("resolve");
        let mut state = MetadataState::new(&base);
        state.set_namespace(
            "",
            "classification",
            BTreeMap::from([("language".into(), serde_json::json!("rust"))]),
        );
        state.set_namespace(
            "src",
            "classification",
            BTreeMap::from([("owner".into(), serde_json::json!("vm"))]),
        );

        let resolved = state.resolve("src");
        let classification = resolved.namespaces.get("classification").unwrap();
        assert_eq!(
            classification.get("language"),
            Some(&serde_json::json!("rust"))
        );
        assert_eq!(classification.get("owner"), Some(&serde_json::json!("vm")));
    }

    #[test]
    fn metadata_save_writes_namespace_shards() {
        let base = temp_path("save");
        let mut state = MetadataState::new(&base);
        state.set_namespace(
            ".",
            "classification",
            BTreeMap::from([("language".into(), serde_json::json!("rust"))]),
        );
        state.set_namespace(
            "src",
            "coding-enrichment-v1",
            BTreeMap::from([("_deep_scan".into(), serde_json::json!({"version": 1}))]),
        );
        state.save().expect("save");

        let metadata_root = crate::runtime_paths::metadata_dir(&base);
        let classification = std::fs::read_to_string(
            metadata_root
                .join("classification")
                .join(NAMESPACE_ENTRIES_FILE),
        )
        .expect("classification shard");
        let parsed = serde_json::from_str::<serde_json::Value>(&classification).expect("json");
        assert_eq!(
            parsed.get("namespace").and_then(|value| value.as_str()),
            Some("classification")
        );
        assert!(parsed
            .get("entries")
            .and_then(|value| value.get("."))
            .is_some());

        let enrichment = std::fs::read_to_string(
            metadata_root
                .join("coding-enrichment-v1")
                .join(NAMESPACE_ENTRIES_FILE),
        )
        .expect("enrichment shard");
        let parsed = serde_json::from_str::<serde_json::Value>(&enrichment).expect("json");
        assert!(parsed
            .get("entries")
            .and_then(|value| value.get("src"))
            .is_some());
    }

    #[test]
    fn metadata_load_merges_legacy_and_namespace_shards() {
        let base = temp_path("load");
        let metadata_root = crate::runtime_paths::metadata_dir(&base);
        std::fs::create_dir_all(metadata_root.join("facts")).unwrap();
        std::fs::write(
            metadata_root.join(LEGACY_SHARD_NAME),
            serde_json::json!({
                "version": 2,
                "entries": {
                    ".": {
                        "namespaces": {
                            "classification": {
                                "language": "rust"
                            }
                        }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            metadata_root.join("facts").join(NAMESPACE_ENTRIES_FILE),
            serde_json::json!({
                "version": 1,
                "namespace": "facts",
                "entries": {
                    "src": {
                        "kind": "module"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let mut state = MetadataState::new(&base);
        state.ensure_loaded();
        assert_eq!(
            state
                .entries
                .get(".")
                .and_then(|meta| meta.namespaces.get("classification"))
                .and_then(|fields| fields.get("language")),
            Some(&serde_json::json!("rust"))
        );
        assert_eq!(
            state
                .entries
                .get("src")
                .and_then(|meta| meta.namespaces.get("facts"))
                .and_then(|fields| fields.get("kind")),
            Some(&serde_json::json!("module"))
        );
    }

    #[test]
    fn scan_options_filter_hidden_and_depth() {
        let base = temp_path("scan");
        std::fs::create_dir_all(base.join("project/deep")).unwrap();
        std::fs::write(base.join("project/root.txt"), "root").unwrap();
        std::fs::write(base.join("project/.hidden.txt"), "hidden").unwrap();
        std::fs::write(base.join("project/deep/nested.txt"), "nested").unwrap();

        let options = ScanOptions {
            pattern: Some(".txt".into()),
            max_depth: 0,
            include_hidden: false,
            include_dirs: false,
            include_files: true,
        };
        let mut results = Vec::new();
        scan_dir_recursive(&base.join("project"), &base, &options, &mut results, 0);
        let paths: Vec<String> = results
            .into_iter()
            .map(|value| match value {
                VmValue::Dict(dict) => dict.get("path").unwrap().display(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(paths, vec!["project/root.txt".to_string()]);
        let _ = std::fs::remove_dir_all(base);
    }
}
