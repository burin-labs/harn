//! Project metadata store for Harn's runtime state root.
//!
//! Provides `metadata_get`, `metadata_set`, `metadata_save`, `metadata_stale`,
//! and `metadata_refresh_hashes` builtins. Stores sharded JSON files by
//! package root.
//!
//! Resolution uses hierarchical inheritance: child directories inherit from
//! parent directories, with overrides at each level.

use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod backend;
mod fingerprint;
mod scan;

use fingerprint::{compute_content_hash_for_dir, compute_structure_hash};

#[cfg(test)]
use scan::{glob_match, ScanOptions};
use scan::{parse_scan_options, resolve_scan_root, scan_dir_recursive};

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;
use backend::{FilesystemMetadataBackend, LoadedEntries, MetadataBackend};

type Namespace = String;
type FieldKey = String;
const LEGACY_SHARD_NAME: &str = "root.json";
const NAMESPACE_ENTRIES_FILE: &str = "entries.json";

/// Per-path metadata: namespaces -> keys -> JSON values. Used for both
/// directory entries (inherited via [`MetadataState::resolve`]) and file
/// entries (exact-path only via [`MetadataState::file_namespace`]).
#[derive(Clone, Default)]
pub(crate) struct PathMetadata {
    pub(crate) namespaces: BTreeMap<Namespace, BTreeMap<FieldKey, serde_json::Value>>,
}

type DirectoryMetadata = PathMetadata;

/// The full metadata store: directory entries (hierarchical) plus file
/// entries (exact-path).
pub(crate) struct MetadataState {
    entries: BTreeMap<String, PathMetadata>,
    files: BTreeMap<String, PathMetadata>,
    base_dir: PathBuf,
    metadata_dir: PathBuf,
    backend: Box<dyn MetadataBackend>,
    loaded: bool,
    dirty: bool,
}

impl MetadataState {
    pub(crate) fn new(base_dir: &Path) -> Self {
        Self::with_metadata_dir(base_dir, crate::runtime_paths::metadata_dir(base_dir))
    }

    fn at_state_root(base_dir: &Path, state_root: &Path) -> Self {
        Self::with_metadata_dir(
            base_dir,
            state_root.join(crate::runtime_paths::METADATA_DIR),
        )
    }

    fn with_metadata_dir(base_dir: &Path, metadata_dir: PathBuf) -> Self {
        Self {
            entries: BTreeMap::new(),
            files: BTreeMap::new(),
            base_dir: base_dir.to_path_buf(),
            metadata_dir,
            backend: Box::new(FilesystemMetadataBackend::new()),
            loaded: false,
            dirty: false,
        }
    }

    fn metadata_dir(&self) -> PathBuf {
        self.metadata_dir.clone()
    }

    fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        self.loaded = true;
        if let Ok(loaded) = self.backend.load(&self.metadata_dir()) {
            self.entries = loaded.dirs;
            self.files = loaded.files;
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
    pub(crate) fn get_namespace(
        &mut self,
        directory: &str,
        namespace: &str,
    ) -> Option<BTreeMap<FieldKey, serde_json::Value>> {
        let resolved = self.resolve(directory);
        resolved.namespaces.get(namespace).cloned()
    }

    pub(crate) fn local_directory(&mut self, directory: &str) -> DirectoryMetadata {
        self.ensure_loaded();
        self.entries.get(directory).cloned().unwrap_or_default()
    }

    fn lineage_directories(&mut self, directory: &str) -> Vec<String> {
        self.ensure_loaded();
        let mut lineage = Vec::new();
        if self.entries.contains_key(".") {
            lineage.push(".".to_string());
        } else if self.entries.contains_key("") {
            lineage.push(String::new());
        }

        let components = directory.split('/').filter(|c| !c.is_empty() && *c != ".");
        let mut current = String::new();
        for component in components {
            if current.is_empty() {
                current = component.to_string();
            } else {
                current = format!("{current}/{component}");
            }
            if self.entries.contains_key(&current) {
                lineage.push(current.clone());
            }
        }
        lineage
    }

    pub(crate) fn origin_directory(
        &mut self,
        directory: &str,
        namespace: &str,
        key: Option<&str>,
    ) -> Option<String> {
        if namespace.is_empty() {
            return None;
        }

        let mut origin = None;
        for candidate in self.lineage_directories(directory) {
            let Some(fields) = self
                .entries
                .get(&candidate)
                .and_then(|metadata| metadata.namespaces.get(namespace))
            else {
                continue;
            };
            if fields.is_empty() {
                continue;
            }
            if let Some(key) = key {
                if fields.contains_key(key) {
                    origin = Some(candidate);
                }
            } else {
                origin = Some(candidate);
            }
        }
        origin
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

    /// Replace a namespace at a directory wholesale. Unlike
    /// [`Self::set_namespace`] (which merges per key), keys absent from
    /// `data` are dropped — required for record-set writers whose payload
    /// is one atomic document, not a key-value accumulation.
    pub(crate) fn replace_namespace(
        &mut self,
        directory: &str,
        namespace: &str,
        data: BTreeMap<FieldKey, serde_json::Value>,
    ) {
        self.ensure_loaded();
        let meta = self.entries.entry(directory.to_string()).or_default();
        meta.namespaces.insert(namespace.to_string(), data);
        self.dirty = true;
    }

    /// Look up file metadata at an exact normalized path. File entries do
    /// not inherit from parent directories.
    fn file_namespace(
        &mut self,
        path: &str,
        namespace: &str,
    ) -> Option<BTreeMap<FieldKey, serde_json::Value>> {
        self.ensure_loaded();
        self.files
            .get(path)
            .and_then(|meta| meta.namespaces.get(namespace).cloned())
    }

    fn file_entry(&mut self, path: &str) -> Option<PathMetadata> {
        self.ensure_loaded();
        self.files.get(path).cloned()
    }

    /// Write file metadata at an exact normalized path.
    fn set_file_namespace(
        &mut self,
        path: &str,
        namespace: &str,
        data: BTreeMap<FieldKey, serde_json::Value>,
    ) {
        self.ensure_loaded();
        let meta = self.files.entry(path.to_string()).or_default();
        let ns = meta.namespaces.entry(namespace.to_string()).or_default();
        for (k, v) in data {
            ns.insert(k, v);
        }
        self.dirty = true;
    }

    /// Save all metadata back to sharded JSON files.
    pub(crate) fn save(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let meta_dir = self.metadata_dir();
        self.backend.save(&meta_dir, &self.entries, &self.files)?;
        self.dirty = false;
        Ok(())
    }
}

/// ISO 8601 timestamp (e.g. `2026-03-29T14:00:00Z`) without a chrono dependency.
pub(crate) fn chrono_now_iso() -> String {
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

fn parse_path_metadata(val: &serde_json::Value) -> PathMetadata {
    let mut meta = PathMetadata::default();
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

fn parse_legacy_entries(contents: &str) -> BTreeMap<String, PathMetadata> {
    let mut entries = BTreeMap::new();
    let parsed: serde_json::Value = match serde_json::from_str(contents) {
        Ok(v) => v,
        Err(_) => return entries,
    };
    let Some(shard_entries) = parsed.get("entries").and_then(|e| e.as_object()) else {
        return entries;
    };
    for (dir, meta_val) in shard_entries {
        entries.insert(dir.clone(), parse_path_metadata(meta_val));
    }
    entries
}

fn merge_namespace_shard(loaded: &mut LoadedEntries, contents: &str) {
    let parsed: serde_json::Value = match serde_json::from_str(contents) {
        Ok(v) => v,
        Err(_) => return,
    };
    let Some(namespace) = parsed.get("namespace").and_then(|value| value.as_str()) else {
        return;
    };
    if let Some(shard_entries) = parsed.get("entries").and_then(|value| value.as_object()) {
        for (dir, fields_val) in shard_entries {
            let directory = loaded.dirs.entry(dir.clone()).or_default();
            directory
                .namespaces
                .insert(namespace.to_string(), parse_namespace_fields(fields_val));
        }
    }
    if let Some(shard_files) = parsed.get("files").and_then(|value| value.as_object()) {
        for (path, fields_val) in shard_files {
            let file = loaded.files.entry(path.clone()).or_default();
            file.namespaces
                .insert(namespace.to_string(), parse_namespace_fields(fields_val));
        }
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

use crate::value::vm_to_storage_json as vm_to_json;

pub(crate) use crate::value::json::json_to_vm_value as json_to_vm;

fn namespace_fields_to_vm(fields: &BTreeMap<FieldKey, serde_json::Value>) -> VmValue {
    let mut map = BTreeMap::new();
    for (k, v) in fields {
        map.insert(k.clone(), json_to_vm(v));
    }
    VmValue::dict(map)
}

fn directory_metadata_to_vm(meta: &DirectoryMetadata) -> VmValue {
    let mut namespaces = BTreeMap::new();
    for (ns, fields) in &meta.namespaces {
        namespaces.insert(ns.clone(), namespace_fields_to_vm(fields));
    }
    VmValue::dict(namespaces)
}

fn normalize_directory_key(dir: &str) -> String {
    if dir.trim().is_empty() || dir == "." {
        ".".to_string()
    } else {
        dir.to_string()
    }
}

/// Normalize a relative path for use as a file metadata key.
///
/// - Converts backslashes to forward slashes.
/// - Strips a leading `./` and a trailing `/` (file keys never end in `/`).
/// - Returns `None` if the result is empty or refers to a directory (`.` or `..`).
fn normalize_file_key(path: &str) -> Option<String> {
    let trimmed = path.trim().replace('\\', "/");
    let stripped = trimmed.strip_prefix("./").unwrap_or(&trimmed);
    let stripped = stripped.trim_end_matches('/');
    if stripped.is_empty() || stripped == "." || stripped == ".." {
        return None;
    }
    Some(stripped.to_string())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathKind {
    File,
    Dir,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PathKindFilter {
    File,
    Dir,
    All,
}

/// Read `opts.kind` from an optional dict argument. Returns `None` on a
/// malformed dict or unknown kind; the empty/nil case yields `default`.
fn parse_path_kind_filter(
    value: Option<&VmValue>,
    default: PathKindFilter,
    allow_all: bool,
) -> Option<PathKindFilter> {
    let dict = match value {
        Some(VmValue::Dict(dict)) => dict,
        Some(VmValue::Nil) | None => return Some(default),
        _ => return None,
    };
    match dict.get("kind") {
        Some(VmValue::String(s)) => match s.as_ref() {
            "file" => Some(PathKindFilter::File),
            "dir" | "directory" => Some(PathKindFilter::Dir),
            "all" if allow_all => Some(PathKindFilter::All),
            _ => None,
        },
        None | Some(VmValue::Nil) => Some(default),
        _ => None,
    }
}

fn parse_path_kind(value: Option<&VmValue>) -> Option<PathKind> {
    match parse_path_kind_filter(value, PathKindFilter::File, false)? {
        PathKindFilter::File => Some(PathKind::File),
        PathKindFilter::Dir => Some(PathKind::Dir),
        PathKindFilter::All => None,
    }
}

/// Register metadata builtins on a VM.
///
/// The per-VM `MetadataState` lives in a thread-local cell so the
/// `#[harn_builtin]`-emitted handler fns can access it without closure
/// capture (which the macro doesn't support). The Harn VM executes
/// single-threaded per run, so each `register_metadata_builtins` call
/// replaces the cell for that thread.
pub fn register_metadata_builtins(vm: &mut Vm, base_dir: &Path) {
    register_metadata_state(vm, MetadataState::new(base_dir));
}

pub(crate) fn register_metadata_builtins_at_state_root(
    vm: &mut Vm,
    base_dir: &Path,
    state_root: &Path,
) {
    register_metadata_state(vm, MetadataState::at_state_root(base_dir, state_root));
}

fn register_metadata_state(vm: &mut Vm, state: MetadataState) {
    METADATA_STATE.with(|cell| {
        *cell.borrow_mut() = Some(state);
    });
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
    // The verification profile store persists through this metadata
    // state, so its builtins register (and become available) exactly
    // when the metadata surface does.
    for def in crate::verification::MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &METADATA_GET_IMPL_DEF,
    &METADATA_RESOLVE_IMPL_DEF,
    &METADATA_ENTRIES_IMPL_DEF,
    &METADATA_SET_IMPL_DEF,
    &METADATA_SAVE_IMPL_DEF,
    &METADATA_STALE_IMPL_DEF,
    &METADATA_REFRESH_HASHES_IMPL_DEF,
    &METADATA_STATUS_IMPL_DEF,
    &COMPUTE_CONTENT_HASH_IMPL_DEF,
    &INVALIDATE_FACTS_IMPL_DEF,
    &PATH_METADATA_GET_IMPL_DEF,
    &PATH_METADATA_SET_IMPL_DEF,
    &PATH_METADATA_ENTRIES_IMPL_DEF,
    &SCAN_DIRECTORY_IMPL_DEF,
];

thread_local! {
    /// Active metadata state for the current thread's pipeline run.
    /// Set by `register_metadata_builtins`; read by the
    /// `#[harn_builtin]` handler fns below. `None` before the first
    /// install — in that case the handlers return a clear runtime error
    /// rather than a panic.
    static METADATA_STATE: RefCell<Option<MetadataState>> = const { RefCell::new(None) };
}

pub(crate) fn with_state<R>(
    fn_name: &'static str,
    f: impl FnOnce(&mut MetadataState) -> Result<R, VmError>,
) -> Result<R, VmError> {
    METADATA_STATE.with(|cell| {
        let mut guard = cell.borrow_mut();
        let state = guard.as_mut().ok_or_else(|| {
            VmError::Runtime(format!(
                "{fn_name}: metadata builtins not registered for this VM"
            ))
        })?;
        f(state)
    })
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "metadata_get(dir: string, namespace?: string|nil) -> dict|nil",
    category = "metadata"
)]
fn metadata_get_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dir = args.first().map(|a| a.display()).unwrap_or_default();
    let namespace = args.get(1).and_then(|a| {
        if matches!(a, VmValue::Nil) {
            None
        } else {
            Some(a.display())
        }
    });

    with_state("metadata_get", |st| {
        if let Some(ns) = namespace {
            match st.get_namespace(&dir, &ns) {
                Some(fields) => {
                    let mut m = BTreeMap::new();
                    for (k, v) in fields {
                        m.insert(k, json_to_vm(&v));
                    }
                    Ok(VmValue::dict(m))
                }
                None => Ok(VmValue::Nil),
            }
        } else {
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
                Ok(VmValue::dict(m))
            }
        }
    })
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "metadata_resolve(dir: string, namespace?: string|nil) -> dict|nil",
    category = "metadata"
)]
fn metadata_resolve_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dir = args.first().map(|a| a.display()).unwrap_or_default();
    let namespace = args.get(1).and_then(|a| {
        if matches!(a, VmValue::Nil) {
            None
        } else {
            Some(a.display())
        }
    });
    with_state("metadata_resolve", |st| {
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
    })
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "metadata_entries(namespace?: string|nil) -> list",
    category = "metadata"
)]
fn metadata_entries_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let namespace = args.first().and_then(|a| {
        if matches!(a, VmValue::Nil) {
            None
        } else {
            Some(a.display())
        }
    });
    with_state("metadata_entries", |st| {
        st.ensure_loaded();
        let directories: Vec<String> = st.entries.keys().cloned().collect();
        let mut items = Vec::new();
        for dir in directories {
            let local = st.local_directory(&dir);
            let resolved = st.resolve(&dir);
            let mut item = BTreeMap::new();
            item.put_str("dir", normalize_directory_key(&dir));
            match &namespace {
                Some(ns) => {
                    item.put_nullable(
                        "local",
                        local.namespaces.get(ns).map(namespace_fields_to_vm),
                    );
                    item.put_nullable(
                        "resolved",
                        resolved.namespaces.get(ns).map(namespace_fields_to_vm),
                    );
                }
                None => {
                    item.insert("local".to_string(), directory_metadata_to_vm(&local));
                    item.insert("resolved".to_string(), directory_metadata_to_vm(&resolved));
                }
            }
            items.push(VmValue::dict(item));
        }
        Ok(VmValue::list(items))
    })
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "metadata_set(dir: string, namespace: string, data: dict) -> nil",
    category = "metadata"
)]
fn metadata_set_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dir = args.first().map(|a| a.display()).unwrap_or_default();
    let namespace = args.get(1).map(|a| a.display()).unwrap_or_default();
    let data_val = args.get(2).cloned().unwrap_or(VmValue::Nil);

    let mut data = BTreeMap::new();
    if let VmValue::Dict(dict) = &data_val {
        for (k, v) in dict.iter() {
            data.insert(k.to_string(), vm_to_json(v)?);
        }
    }

    with_state("metadata_set", |st| {
        if !data.is_empty() {
            st.set_namespace(&dir, &namespace, data);
        }
        Ok(VmValue::Nil)
    })
}

fn metadata_fields_from_value(
    namespace: &str,
    value: &VmValue,
) -> Result<BTreeMap<FieldKey, serde_json::Value>, VmError> {
    let mut data = BTreeMap::new();
    match value {
        VmValue::Dict(dict) => {
            for (k, v) in dict.iter() {
                data.insert(k.to_string(), vm_to_json(v)?);
            }
        }
        VmValue::String(_) if !namespace.is_empty() => {
            data.insert(namespace.to_string(), vm_to_json(value)?);
        }
        _ => {}
    }
    Ok(data)
}

fn classification_field<'a>(
    metadata: &'a DirectoryMetadata,
    key: &str,
) -> Option<&'a serde_json::Value> {
    metadata
        .namespaces
        .get("classification")
        .and_then(|classification| classification.get(key))
}

fn classification_string(metadata: &DirectoryMetadata, key: &str) -> Option<String> {
    classification_field(metadata, key)
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
}

fn metadata_full_dir(base: &Path, dir: &str) -> PathBuf {
    if dir.is_empty() || dir == "." {
        base.to_path_buf()
    } else {
        base.join(dir)
    }
}

fn metadata_inspect_stale_flags(
    base: &Path,
    dir: &str,
    metadata: &DirectoryMetadata,
) -> (bool, bool) {
    let full_dir = metadata_full_dir(base, dir);
    if let Some(stored_hash) = classification_string(metadata, "structureHash") {
        let current_hash = compute_structure_hash(&full_dir);
        if !current_hash.is_ok_and(|hash| hash == stored_hash) {
            return (true, true);
        }
    }
    if let Some(stored_hash) = classification_string(metadata, "contentHash") {
        let current_hash = compute_content_hash_for_dir(&full_dir);
        if !current_hash.is_ok_and(|hash| hash == stored_hash) {
            return (true, true);
        }
    }
    (false, false)
}

fn optional_string_value(value: Option<String>) -> VmValue {
    value.map(VmValue::string).unwrap_or(VmValue::Nil)
}

fn refresh_metadata_hashes(st: &mut MetadataState) -> Result<(), VmError> {
    st.ensure_loaded();
    let base = st.base_dir.clone();
    let dirs: Vec<String> = st.entries.keys().cloned().collect();
    let hashes = dirs
        .into_iter()
        .map(|dir| {
            let full_dir = metadata_full_dir(&base, &dir);
            match compute_structure_hash(&full_dir) {
                Ok(hash) => Ok(Some((dir, hash))),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    Ok(None)
                }
                Err(error) => Err(fingerprint_error(&full_dir, error)),
            }
        })
        .collect::<Result<Vec<_>, VmError>>()?;
    for (dir, hash) in hashes.into_iter().flatten() {
        let entry = st.entries.entry(dir).or_default();
        let ns = entry
            .namespaces
            .entry("classification".to_string())
            .or_default();
        ns.insert("structureHash".to_string(), serde_json::Value::String(hash));
    }
    st.dirty = true;
    Ok(())
}

fn fingerprint_error(path: &Path, error: std::io::Error) -> VmError {
    VmError::Runtime(format!(
        "cannot fingerprint directory {}: {error}",
        path.display()
    ))
}

fn param_string(params: &crate::value::DictMap, key: &str) -> String {
    params
        .get(key)
        .map(|value| value.display())
        .unwrap_or_default()
}

fn optional_param_string(params: &crate::value::DictMap, key: &str) -> Option<String> {
    let value = param_string(params, key);
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Standalone `host_call("project.metadata_get", ...)` fallback.
///
/// Embedders such as Burin may provide their own bridge-backed project metadata
/// store. When no bridge handles the call, route the host-shaped operation to
/// Harn's built-in metadata store so standalone agent/debug/eval runs keep the
/// same cross-run learning substrate instead of failing closed.
pub(crate) fn project_metadata_host_get(
    params: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    let dir = param_string(params, "dir");
    let namespace = optional_param_string(params, "namespace");
    with_state("project.metadata_get", |st| {
        if let Some(ns) = namespace {
            match st.get_namespace(&dir, &ns) {
                Some(fields) => Ok(namespace_fields_to_vm(&fields)),
                None => Ok(VmValue::Nil),
            }
        } else {
            let resolved = st.resolve(&dir);
            if resolved.namespaces.is_empty() {
                Ok(VmValue::Nil)
            } else {
                Ok(directory_metadata_to_vm(&resolved))
            }
        }
    })
}

/// Standalone `host_call("project.metadata_inspect", ...)` fallback.
pub(crate) fn project_metadata_host_inspect(
    params: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    let dir = param_string(params, "dir");
    let namespace = param_string(params, "namespace");
    let key = optional_param_string(params, "key");
    with_state("project.metadata_inspect", |st| {
        let resolved = st.resolve(&dir);
        let origin = st.origin_directory(&dir, &namespace, key.as_deref());
        let inspection_dir = origin.clone().unwrap_or_else(|| dir.clone());
        let missing_structure_hash = classification_string(&resolved, "structureHash").is_none();
        let missing_content_hash = classification_string(&resolved, "contentHash").is_none();
        let (stale_tier1, stale_tier2) = if missing_structure_hash || missing_content_hash {
            (false, false)
        } else {
            metadata_inspect_stale_flags(&st.base_dir, &inspection_dir, &resolved)
        };

        let mut payload = BTreeMap::new();
        payload.put_str("dir", dir);
        payload.put_str("namespace", namespace.clone());
        payload.insert(
            "origin_dir".to_string(),
            optional_string_value(origin.clone().map(|raw| normalize_directory_key(&raw))),
        );
        payload.put_str("inspected_dir", normalize_directory_key(&inspection_dir));
        payload.put_bool("missing_structure_hash", missing_structure_hash);
        payload.put_bool("missing_content_hash", missing_content_hash);
        payload.put_bool("stale_tier1", stale_tier1);
        payload.put_bool("stale_tier2", stale_tier2);
        let enriched_at = classification_field(&resolved, "enrichedAt");
        payload.put_bool("has_enriched_at", enriched_at.is_some());
        payload.put_nullable("enriched_at_ms", enriched_at.map(json_to_vm));
        payload.insert(
            "structure_hash".to_string(),
            optional_string_value(classification_string(&resolved, "structureHash")),
        );
        payload.insert(
            "content_hash".to_string(),
            optional_string_value(classification_string(&resolved, "contentHash")),
        );

        if namespace.is_empty() {
            payload.insert("resolved".to_string(), directory_metadata_to_vm(&resolved));
            payload.put("origin", VmValue::Nil);
        } else {
            let resolved_fields = resolved
                .namespaces
                .get(&namespace)
                .map(namespace_fields_to_vm)
                .unwrap_or(VmValue::dict(crate::value::DictMap::new()));
            payload.insert("resolved".to_string(), resolved_fields);
            let origin_fields = origin
                .as_ref()
                .and_then(|origin_dir| st.entries.get(origin_dir))
                .and_then(|metadata| metadata.namespaces.get(&namespace))
                .map(namespace_fields_to_vm)
                .unwrap_or(VmValue::Nil);
            payload.insert("origin".to_string(), origin_fields);
        }
        Ok(VmValue::dict(payload))
    })
}

/// Standalone `host_call("project.metadata_set", ...)` fallback.
pub(crate) fn project_metadata_host_set(
    params: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    let dir = param_string(params, "dir");
    let namespace = param_string(params, "namespace");
    let value = params
        .get("value")
        .or_else(|| params.get("data"))
        .cloned()
        .unwrap_or(VmValue::Nil);
    let data = metadata_fields_from_value(&namespace, &value)?;
    if namespace.is_empty() || data.is_empty() {
        return Ok(VmValue::Nil);
    }
    with_state("project.metadata_set", |st| {
        st.set_namespace(&dir, &namespace, data);
        st.save().map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    })
}

/// Standalone `host_call("project.metadata_save", ...)` fallback.
pub(crate) fn project_metadata_host_save(
    _params: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    with_state("project.metadata_save", |st| {
        st.save().map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    })
}

/// Standalone `host_call("project.metadata_stale", ...)` fallback.
pub(crate) fn project_metadata_host_stale(
    _params: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    with_state("project.metadata_stale", |st| {
        st.ensure_loaded();
        let base = st.base_dir.clone();
        Ok(metadata_stale_value(st, &base))
    })
}

/// Standalone `host_call("project.metadata_refresh_hashes", ...)` fallback.
pub(crate) fn project_metadata_host_refresh_hashes(
    _params: &crate::value::DictMap,
) -> Result<VmValue, VmError> {
    with_state("project.metadata_refresh_hashes", |st| {
        refresh_metadata_hashes(st)?;
        st.save().map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    })
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "metadata_save() -> nil",
    category = "metadata"
)]
fn metadata_save_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    with_state("metadata_save", |st| {
        st.save().map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    })
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "metadata_stale(project?: string) -> dict",
    category = "metadata"
)]
fn metadata_stale_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    with_state("metadata_stale", |st| {
        st.ensure_loaded();
        let base = st.base_dir.clone();
        Ok(metadata_stale_value(st, &base))
    })
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "metadata_refresh_hashes() -> nil",
    category = "metadata"
)]
fn metadata_refresh_hashes_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    with_state("metadata_refresh_hashes", |st| {
        refresh_metadata_hashes(st)?;
        Ok(VmValue::Nil)
    })
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "metadata_status(namespace?: string|nil) -> dict",
    category = "metadata"
)]
fn metadata_status_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let namespace = args.first().and_then(|a| {
        if matches!(a, VmValue::Nil) {
            None
        } else {
            Some(a.display())
        }
    });
    with_state("metadata_status", |st| {
        st.ensure_loaded();
        let base = st.base_dir.clone();
        let mut namespaces = BTreeMap::new();
        let mut directories = Vec::new();
        let mut missing_structure_hash = Vec::new();
        let mut missing_content_hash = Vec::new();
        for (dir, meta) in &st.entries {
            directories.push(VmValue::string(normalize_directory_key(dir)));
            for ns in meta.namespaces.keys() {
                namespaces.insert(ns.clone(), VmValue::Bool(true));
            }
            let full_dir = if dir.is_empty() {
                base.clone()
            } else {
                base.join(dir)
            };
            let relevant = namespace
                .as_ref()
                .and_then(|name| meta.namespaces.get(name))
                .or_else(|| meta.namespaces.get("classification"));
            if let Some(fields) = relevant {
                if !fields.contains_key("structureHash") && full_dir.exists() {
                    missing_structure_hash.push(VmValue::string(normalize_directory_key(dir)));
                }
                if !fields.contains_key("contentHash") && full_dir.exists() {
                    missing_content_hash.push(VmValue::string(normalize_directory_key(dir)));
                }
            }
        }
        let stale = metadata_stale_value(st, &base);
        let mut result = BTreeMap::new();
        result.put_int("directory_count", st.entries.len() as i64);
        result.put_int("namespace_count", namespaces.len() as i64);
        result.put("namespaces", VmValue::strings(namespaces.keys().cloned()));
        result.put("directories", VmValue::list(directories));
        result.put(
            "missing_structure_hash",
            VmValue::list(missing_structure_hash),
        );
        result.put("missing_content_hash", VmValue::list(missing_content_hash));
        result.insert("stale".to_string(), stale);
        Ok(VmValue::dict(result))
    })
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "compute_content_hash(dir: string) -> string",
    category = "metadata"
)]
fn compute_content_hash_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dir = args.first().map(|a| a.display()).unwrap_or_default();
    with_state("compute_content_hash", |st| {
        let full_dir = if dir.is_empty() {
            st.base_dir.clone()
        } else {
            st.base_dir.join(&dir)
        };
        let hash = compute_content_hash_for_dir(&full_dir)
            .map_err(|error| fingerprint_error(&full_dir, error))?;
        Ok(VmValue::string(hash))
    })
}

/// invalidate_facts is a no-op: facts live in the metadata namespace.
#[harn_builtin(
    exposure = "runtime_internal",
    sig = "invalidate_facts(dir?: string) -> nil",
    category = "metadata"
)]
fn invalidate_facts_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Nil)
}

/// Reads metadata for an exact path.
/// Files do not inherit; `{kind: "dir"}` enables hierarchical resolution.
#[harn_builtin(
    exposure = "runtime_internal",
    sig = "path_metadata_get(path: string, namespace?: string|nil, opts?: dict|nil) -> dict|nil",
    category = "metadata"
)]
fn path_metadata_get_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let namespace = args.get(1).and_then(|a| {
        if matches!(a, VmValue::Nil) {
            None
        } else {
            Some(a.display())
        }
    });
    let Some(kind) = parse_path_kind(args.get(2)) else {
        return Err(VmError::Runtime(
            "path_metadata_get: opts.kind must be \"file\" or \"dir\"".to_string(),
        ));
    };
    with_state("path_metadata_get", |st| match kind {
        PathKind::File => {
            let Some(key) = normalize_file_key(&path) else {
                return Ok(VmValue::Nil);
            };
            match namespace {
                Some(ns) => match st.file_namespace(&key, &ns) {
                    Some(fields) => Ok(namespace_fields_to_vm(&fields)),
                    None => Ok(VmValue::Nil),
                },
                None => match st.file_entry(&key) {
                    Some(meta) if !meta.namespaces.is_empty() => {
                        Ok(directory_metadata_to_vm(&meta))
                    }
                    _ => Ok(VmValue::Nil),
                },
            }
        }
        PathKind::Dir => {
            if let Some(ns) = namespace {
                match st.get_namespace(&path, &ns) {
                    Some(fields) => Ok(namespace_fields_to_vm(&fields)),
                    None => Ok(VmValue::Nil),
                }
            } else {
                let resolved = st.resolve(&path);
                if resolved.namespaces.is_empty() {
                    Ok(VmValue::Nil)
                } else {
                    Ok(directory_metadata_to_vm(&resolved))
                }
            }
        }
    })
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "path_metadata_set(path: string, namespace: string, data: dict, opts?: dict|nil) -> nil",
    category = "metadata"
)]
fn path_metadata_set_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let namespace = args.get(1).map(|a| a.display()).unwrap_or_default();
    let data_val = args.get(2).cloned().unwrap_or(VmValue::Nil);
    let Some(kind) = parse_path_kind(args.get(3)) else {
        return Err(VmError::Runtime(
            "path_metadata_set: opts.kind must be \"file\" or \"dir\"".to_string(),
        ));
    };
    if namespace.is_empty() {
        return Err(VmError::Runtime(
            "path_metadata_set: namespace must not be empty".to_string(),
        ));
    }
    let mut data = BTreeMap::new();
    if let VmValue::Dict(dict) = &data_val {
        for (k, v) in dict.iter() {
            data.insert(k.to_string(), vm_to_json(v)?);
        }
    }
    if data.is_empty() {
        return Ok(VmValue::Nil);
    }
    with_state("path_metadata_set", |st| {
        match kind {
            PathKind::File => {
                let Some(key) = normalize_file_key(&path) else {
                    return Err(VmError::Runtime(format!(
                        "path_metadata_set: {path:?} is not a valid file path"
                    )));
                };
                st.set_file_namespace(&key, &namespace, data);
            }
            PathKind::Dir => {
                st.set_namespace(&path, &namespace, data);
            }
        }
        st.save().map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    })
}

/// Lists stored file (and optionally directory) entries. Useful for
/// iterating over precomputed enrichment artifacts.
#[harn_builtin(
    exposure = "runtime_internal",
    sig = "path_metadata_entries(namespace?: string|nil, opts?: dict|nil) -> list",
    category = "metadata"
)]
fn path_metadata_entries_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let namespace = args.first().and_then(|a| {
        if matches!(a, VmValue::Nil) {
            None
        } else {
            Some(a.display())
        }
    });
    let Some(filter) = parse_path_kind_filter(args.get(1), PathKindFilter::File, true) else {
        return Err(VmError::Runtime(
            "path_metadata_entries: opts.kind must be \"file\", \"dir\", or \"all\"".to_string(),
        ));
    };
    let include_files = matches!(filter, PathKindFilter::File | PathKindFilter::All);
    let include_dirs = matches!(filter, PathKindFilter::Dir | PathKindFilter::All);
    with_state("path_metadata_entries", |st| {
        st.ensure_loaded();
        let mut items = Vec::new();
        if include_files {
            for (path, meta) in &st.files {
                let local = match &namespace {
                    Some(ns) => match meta.namespaces.get(ns) {
                        Some(fields) => namespace_fields_to_vm(fields),
                        None => continue,
                    },
                    None => directory_metadata_to_vm(meta),
                };
                let mut item = BTreeMap::new();
                item.put_str("kind", "file");
                item.put_str("path", path.as_str());
                item.insert("local".to_string(), local);
                items.push(VmValue::dict(item));
            }
        }
        if include_dirs {
            let directories: Vec<String> = st.entries.keys().cloned().collect();
            for dir in directories {
                let local = st.local_directory(&dir);
                let resolved = st.resolve(&dir);
                let local_value = match &namespace {
                    Some(ns) => match local.namespaces.get(ns) {
                        Some(fields) => namespace_fields_to_vm(fields),
                        None => continue,
                    },
                    None => directory_metadata_to_vm(&local),
                };
                let resolved_value = match &namespace {
                    Some(ns) => resolved
                        .namespaces
                        .get(ns)
                        .map(namespace_fields_to_vm)
                        .unwrap_or(VmValue::Nil),
                    None => directory_metadata_to_vm(&resolved),
                };
                let mut item = BTreeMap::new();
                item.put_str("kind", "dir");
                item.put_str("path", normalize_directory_key(&dir));
                item.insert("local".to_string(), local_value);
                item.insert("resolved".to_string(), resolved_value);
                items.push(VmValue::dict(item));
            }
        }
        Ok(VmValue::list(items))
    })
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "scan_directory(path?: string, pattern_or_options?: string|dict|nil, options?: dict|nil) -> list",
    category = "metadata"
)]
fn scan_directory_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
    Ok(VmValue::list(results))
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
            if !current_hash.is_ok_and(|hash| hash == stored_hash) {
                tier1_stale.push(VmValue::string(normalize_directory_key(dir)));
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
            if !current_hash.is_ok_and(|hash| hash == stored_hash) {
                tier2_stale.push(VmValue::string(normalize_directory_key(dir)));
            }
        }
    }
    let any_stale = !tier1_stale.is_empty() || !tier2_stale.is_empty();
    let mut m = BTreeMap::new();
    m.put_bool("any_stale", any_stale);
    m.put("tier1", VmValue::list(tier1_stale));
    m.put("tier2", VmValue::list(tier2_stale));
    VmValue::dict(m)
}

#[cfg(test)]
mod tests;
