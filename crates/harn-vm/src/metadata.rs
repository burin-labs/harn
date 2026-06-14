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

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

type Namespace = String;
type FieldKey = String;
const LEGACY_SHARD_NAME: &str = "root.json";
const NAMESPACE_ENTRIES_FILE: &str = "entries.json";

/// Per-path metadata: namespaces -> keys -> JSON values. Used for both
/// directory entries (inherited via [`MetadataState::resolve`]) and file
/// entries (exact-path only via [`MetadataState::file_namespace`]).
#[derive(Clone, Default)]
struct PathMetadata {
    namespaces: BTreeMap<Namespace, BTreeMap<FieldKey, serde_json::Value>>,
}

type DirectoryMetadata = PathMetadata;

/// Loaded form of a namespace shard: directory entries plus file entries
/// keyed by normalized relative path. File entries do not inherit.
#[derive(Default)]
struct LoadedEntries {
    dirs: BTreeMap<String, PathMetadata>,
    files: BTreeMap<String, PathMetadata>,
}

trait MetadataBackend {
    fn backend_name(&self) -> &'static str;
    fn load(&self, root: &Path) -> Result<LoadedEntries, String>;
    fn save(
        &self,
        root: &Path,
        dirs: &BTreeMap<String, PathMetadata>,
        files: &BTreeMap<String, PathMetadata>,
    ) -> Result<(), String>;
}

#[derive(Default)]
struct FilesystemMetadataBackend;

impl FilesystemMetadataBackend {
    fn new() -> Self {
        Self
    }
}

/// The full metadata store: directory entries (hierarchical) plus file
/// entries (exact-path).
struct MetadataState {
    entries: BTreeMap<String, PathMetadata>,
    files: BTreeMap<String, PathMetadata>,
    base_dir: PathBuf,
    backend: Box<dyn MetadataBackend>,
    loaded: bool,
    dirty: bool,
}

impl MetadataState {
    fn new(base_dir: &Path) -> Self {
        Self {
            entries: BTreeMap::new(),
            files: BTreeMap::new(),
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
    fn save(&mut self) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        let meta_dir = self.metadata_dir();
        self.backend.save(&meta_dir, &self.entries, &self.files)?;
        self.dirty = false;
        Ok(())
    }
}

impl MetadataBackend for FilesystemMetadataBackend {
    fn backend_name(&self) -> &'static str {
        "filesystem"
    }

    fn load(&self, root: &Path) -> Result<LoadedEntries, String> {
        let mut loaded = LoadedEntries::default();
        let legacy_path = root.join(LEGACY_SHARD_NAME);
        if let Ok(contents) = std::fs::read_to_string(&legacy_path) {
            loaded.dirs = parse_legacy_entries(&contents);
        }

        let namespace_dirs = match std::fs::read_dir(root) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(loaded),
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
            merge_namespace_shard(&mut loaded, &contents);
        }

        Ok(loaded)
    }

    fn save(
        &self,
        root: &Path,
        dirs: &BTreeMap<String, PathMetadata>,
        files: &BTreeMap<String, PathMetadata>,
    ) -> Result<(), String> {
        std::fs::create_dir_all(root).map_err(|error| format!("metadata mkdir: {error}"))?;

        let mut dir_namespaces: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
            BTreeMap::new();
        for (dir, meta) in dirs {
            for (namespace, fields) in &meta.namespaces {
                dir_namespaces
                    .entry(namespace.clone())
                    .or_default()
                    .insert(dir.clone(), serialize_namespace_fields(fields));
            }
        }
        let mut file_namespaces: BTreeMap<String, serde_json::Map<String, serde_json::Value>> =
            BTreeMap::new();
        for (path, meta) in files {
            for (namespace, fields) in &meta.namespaces {
                file_namespaces
                    .entry(namespace.clone())
                    .or_default()
                    .insert(path.clone(), serialize_namespace_fields(fields));
            }
        }

        let mut all_namespaces: std::collections::BTreeSet<String> =
            dir_namespaces.keys().cloned().collect();
        all_namespaces.extend(file_namespaces.keys().cloned());

        for namespace in all_namespaces {
            let dir_entries = dir_namespaces.remove(&namespace).unwrap_or_default();
            let file_entries = file_namespaces.remove(&namespace).unwrap_or_default();
            let namespace_dir = root.join(namespace_path_component(&namespace));
            std::fs::create_dir_all(&namespace_dir)
                .map_err(|error| format!("metadata mkdir: {error}"))?;
            let mut shard = serde_json::Map::new();
            shard.insert("version".to_string(), serde_json::json!(1));
            shard.insert(
                "namespace".to_string(),
                serde_json::Value::String(namespace.clone()),
            );
            shard.insert(
                "backend".to_string(),
                serde_json::Value::String(self.backend_name().to_string()),
            );
            shard.insert(
                "generatedAt".to_string(),
                serde_json::Value::String(chrono_now_iso()),
            );
            shard.insert(
                "entries".to_string(),
                serde_json::Value::Object(dir_entries),
            );
            if !file_entries.is_empty() {
                shard.insert("files".to_string(), serde_json::Value::Object(file_entries));
            }
            let json = serde_json::to_string_pretty(&serde_json::Value::Object(shard))
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
        serde_json::Value::String(s) => VmValue::String(std::sync::Arc::from(s.as_str())),
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

fn bool_arg(map: &crate::value::DictMap, key: &str, default: bool) -> bool {
    match map.get(key) {
        Some(VmValue::Bool(value)) => *value,
        _ => default,
    }
}

fn usize_arg(map: &crate::value::DictMap, key: &str, default: usize) -> usize {
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

fn apply_scan_options_dict(options: &mut ScanOptions, dict: &crate::value::DictMap) {
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
/// The per-VM `MetadataState` lives in a thread-local cell so the
/// `#[harn_builtin]`-emitted handler fns can access it without closure
/// capture (which the macro doesn't support). The Harn VM executes
/// single-threaded per run, so each `register_metadata_builtins` call
/// replaces the cell for that thread.
pub fn register_metadata_builtins(vm: &mut Vm, base_dir: &Path) {
    METADATA_STATE.with(|cell| {
        *cell.borrow_mut() = Some(MetadataState::new(base_dir));
    });
    for def in MODULE_BUILTINS {
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

fn with_state<R>(
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
            items.push(VmValue::dict(item));
        }
        Ok(VmValue::List(std::sync::Arc::new(items)))
    })
}

#[harn_builtin(
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
            data.insert(k.clone(), vm_to_json(v));
        }
    }

    with_state("metadata_set", |st| {
        if !data.is_empty() {
            st.set_namespace(&dir, &namespace, data);
        }
        Ok(VmValue::Nil)
    })
}

#[harn_builtin(sig = "metadata_save() -> nil", category = "metadata")]
fn metadata_save_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    with_state("metadata_save", |st| {
        st.save().map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    })
}

#[harn_builtin(
    sig = "metadata_stale(project?: string) -> dict",
    category = "metadata"
)]
fn metadata_stale_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    with_state("metadata_stale", |st| {
        st.ensure_loaded();
        let base = st.base_dir.clone();
        let mut tier1_stale: Vec<VmValue> = Vec::new();
        let mut tier2_stale: Vec<VmValue> = Vec::new();

        for (dir, meta) in &st.entries {
            let full_dir = if dir.is_empty() {
                base.clone()
            } else {
                base.join(dir)
            };
            if let Some(stored_hash) = meta
                .namespaces
                .get("classification")
                .and_then(|ns| ns.get("structureHash"))
                .and_then(|v| v.as_str())
            {
                let current_hash = compute_structure_hash(&full_dir);
                if current_hash != stored_hash {
                    tier1_stale.push(VmValue::String(std::sync::Arc::from(dir.as_str())));
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
                    tier2_stale.push(VmValue::String(std::sync::Arc::from(dir.as_str())));
                }
            }
        }

        let any_stale = !tier1_stale.is_empty() || !tier2_stale.is_empty();
        let mut m = BTreeMap::new();
        m.insert("any_stale".to_string(), VmValue::Bool(any_stale));
        m.insert(
            "tier1".to_string(),
            VmValue::List(std::sync::Arc::new(tier1_stale)),
        );
        m.insert(
            "tier2".to_string(),
            VmValue::List(std::sync::Arc::new(tier2_stale)),
        );
        Ok(VmValue::dict(m))
    })
}

#[harn_builtin(sig = "metadata_refresh_hashes() -> nil", category = "metadata")]
fn metadata_refresh_hashes_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    with_state("metadata_refresh_hashes", |st| {
        st.ensure_loaded();
        let base = st.base_dir.clone();
        let dirs: Vec<String> = st.entries.keys().cloned().collect();
        for dir in dirs {
            let full_dir = if dir.is_empty() {
                base.clone()
            } else {
                base.join(&dir)
            };
            let hash = compute_structure_hash(&full_dir);
            let entry = st.entries.entry(dir).or_default();
            let ns = entry
                .namespaces
                .entry("classification".to_string())
                .or_default();
            ns.insert("structureHash".to_string(), serde_json::Value::String(hash));
        }
        st.dirty = true;
        Ok(VmValue::Nil)
    })
}

#[harn_builtin(
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
            directories.push(VmValue::String(std::sync::Arc::from(
                normalize_directory_key(dir),
            )));
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
                    missing_structure_hash.push(VmValue::String(std::sync::Arc::from(
                        normalize_directory_key(dir),
                    )));
                }
                if !fields.contains_key("contentHash") && full_dir.exists() {
                    missing_content_hash.push(VmValue::String(std::sync::Arc::from(
                        normalize_directory_key(dir),
                    )));
                }
            }
        }
        let stale = metadata_stale_value(st, &base);
        let mut result = BTreeMap::new();
        result.insert(
            "directory_count".to_string(),
            VmValue::Int(st.entries.len() as i64),
        );
        result.insert(
            "namespace_count".to_string(),
            VmValue::Int(namespaces.len() as i64),
        );
        result.insert(
            "namespaces".to_string(),
            VmValue::List(std::sync::Arc::new(
                namespaces
                    .keys()
                    .cloned()
                    .map(|name| VmValue::String(std::sync::Arc::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "directories".to_string(),
            VmValue::List(std::sync::Arc::new(directories)),
        );
        result.insert(
            "missing_structure_hash".to_string(),
            VmValue::List(std::sync::Arc::new(missing_structure_hash)),
        );
        result.insert(
            "missing_content_hash".to_string(),
            VmValue::List(std::sync::Arc::new(missing_content_hash)),
        );
        result.insert("stale".to_string(), stale);
        Ok(VmValue::dict(result))
    })
}

#[harn_builtin(
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
        let hash = compute_content_hash_for_dir(&full_dir);
        Ok(VmValue::String(std::sync::Arc::from(hash)))
    })
}

/// invalidate_facts is a no-op: facts live in the metadata namespace.
#[harn_builtin(sig = "invalidate_facts(dir?: string) -> nil", category = "metadata")]
fn invalidate_facts_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Nil)
}

/// Reads metadata for an exact path. Files are addressed directly without
/// inheritance from parent directories. Pass `{kind: "dir"}` to fall back
/// to hierarchical directory resolution.
#[harn_builtin(
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
            data.insert(k.clone(), vm_to_json(v));
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
        Ok(VmValue::Nil)
    })
}

/// Lists stored file (and optionally directory) entries. Useful for
/// iterating over precomputed enrichment artifacts.
#[harn_builtin(
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
        Ok(VmValue::List(std::sync::Arc::new(items)))
    })
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

#[harn_builtin(
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
    Ok(VmValue::List(std::sync::Arc::new(results)))
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
                tier1_stale.push(VmValue::String(std::sync::Arc::from(
                    normalize_directory_key(dir),
                )));
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
                tier2_stale.push(VmValue::String(std::sync::Arc::from(
                    normalize_directory_key(dir),
                )));
            }
        }
    }
    let any_stale = !tier1_stale.is_empty() || !tier2_stale.is_empty();
    let mut m = BTreeMap::new();
    m.insert("any_stale".to_string(), VmValue::Bool(any_stale));
    m.insert(
        "tier1".to_string(),
        VmValue::List(std::sync::Arc::new(tier1_stale)),
    );
    m.insert(
        "tier2".to_string(),
        VmValue::List(std::sync::Arc::new(tier2_stale)),
    );
    VmValue::dict(m)
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
            .replace('\\', "/");
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
        m.put_str("path", rel_path);
        m.insert("size".to_string(), VmValue::Int(meta.len() as i64));
        m.insert("modified".to_string(), VmValue::Int(mtime));
        m.insert("is_dir".to_string(), VmValue::Bool(meta.is_dir()));
        if (meta.is_dir() && options.include_dirs) || (!meta.is_dir() && options.include_files) {
            results.push(VmValue::dict(m));
        }
        if meta.is_dir() {
            scan_dir_recursive(&entry.path(), base, options, results, depth + 1);
        }
    }
}

/// Scan-pattern matching. Patterns with wildcards use the shared glob
/// matcher in name mode (`*` crosses `/`, so the historical `*.rs` form keeps
/// matching nested entries during the recursive scan); wildcard-free patterns
/// keep their historical substring semantics.
fn glob_match(pattern: &str, path: &str) -> bool {
    if pattern.contains('*') || pattern.contains('?') {
        harn_glob::match_name(pattern, path)
    } else {
        path.contains(pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_pattern_matching_globs_and_substrings() {
        // Historical recursive-scan form: `*` crosses directory separators.
        assert!(glob_match("*.rs", "src/nested/main.rs"));
        // `**` forms match too (the previous hand-rolled matcher rejected
        // `**/*.rs` because it compared the second `*` literally).
        assert!(glob_match("**/*.rs", "src/nested/main.rs"));
        assert!(glob_match("src/**", "src/nested/main.rs"));
        assert!(!glob_match("*.toml", "src/main.rs"));
        // Wildcard-free patterns keep substring semantics.
        assert!(glob_match("nested", "src/nested/main.rs"));
        assert!(!glob_match("missing", "src/nested/main.rs"));
    }

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
    fn path_metadata_file_round_trip_does_not_inherit() {
        let base = temp_path("path_file_roundtrip");
        let mut state = MetadataState::new(&base);

        // Dir-level fact set on a parent — should not leak into file lookup.
        state.set_namespace(
            "src",
            "facts",
            BTreeMap::from([("owner".into(), serde_json::json!("vm"))]),
        );
        state.set_file_namespace(
            "src/foo.rs",
            "facts",
            BTreeMap::from([("summary".into(), serde_json::json!("entry point"))]),
        );

        let file_fields = state.file_namespace("src/foo.rs", "facts").expect("file");
        assert_eq!(
            file_fields.get("summary"),
            Some(&serde_json::json!("entry point"))
        );
        // File lookup must NOT inherit "owner" from the parent dir entry.
        assert!(!file_fields.contains_key("owner"));

        // Missing file path returns None.
        assert!(state.file_namespace("src/missing.rs", "facts").is_none());
        // Missing namespace on a known file returns None.
        assert!(state
            .file_namespace("src/foo.rs", "other_namespace")
            .is_none());
    }

    #[test]
    fn path_metadata_persists_files_alongside_dirs() {
        let base = temp_path("path_persist");
        let mut state = MetadataState::new(&base);
        state.set_namespace(
            ".",
            "classification",
            BTreeMap::from([("language".into(), serde_json::json!("rust"))]),
        );
        state.set_file_namespace(
            "src/foo.rs",
            "facts",
            BTreeMap::from([("summary".into(), serde_json::json!("entry point"))]),
        );
        state.set_file_namespace(
            "src/bar.rs",
            "facts",
            BTreeMap::from([("summary".into(), serde_json::json!("helpers"))]),
        );
        state.save().expect("save");

        let facts_shard = std::fs::read_to_string(
            crate::runtime_paths::metadata_dir(&base)
                .join("facts")
                .join(NAMESPACE_ENTRIES_FILE),
        )
        .expect("facts shard");
        let parsed = serde_json::from_str::<serde_json::Value>(&facts_shard).expect("json");
        let files = parsed.get("files").and_then(|v| v.as_object()).unwrap();
        assert!(files.contains_key("src/foo.rs"));
        assert!(files.contains_key("src/bar.rs"));

        // Dir-only namespace must not write a `files` field.
        let class_shard = std::fs::read_to_string(
            crate::runtime_paths::metadata_dir(&base)
                .join("classification")
                .join(NAMESPACE_ENTRIES_FILE),
        )
        .expect("classification shard");
        let parsed = serde_json::from_str::<serde_json::Value>(&class_shard).expect("json");
        assert!(parsed.get("files").is_none());

        // Reload from disk and verify file entries round-trip.
        let mut reloaded = MetadataState::new(&base);
        let fields = reloaded
            .file_namespace("src/foo.rs", "facts")
            .expect("reloaded");
        assert_eq!(
            fields.get("summary"),
            Some(&serde_json::json!("entry point"))
        );
    }

    #[test]
    fn path_metadata_load_tolerates_stale_snapshot_without_files_section() {
        let base = temp_path("path_stale");
        let metadata_root = crate::runtime_paths::metadata_dir(&base);
        std::fs::create_dir_all(metadata_root.join("facts")).unwrap();
        // Pre-v2 shard with only `entries`, no `files` — must still load.
        std::fs::write(
            metadata_root.join("facts").join(NAMESPACE_ENTRIES_FILE),
            serde_json::json!({
                "version": 1,
                "namespace": "facts",
                "entries": {
                    "src": {"kind": "module"}
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
                .get("src")
                .and_then(|meta| meta.namespaces.get("facts"))
                .and_then(|f| f.get("kind")),
            Some(&serde_json::json!("module"))
        );
        assert!(state.files.is_empty());
        assert!(state.file_namespace("src/foo.rs", "facts").is_none());
    }

    #[test]
    fn normalize_file_key_handles_common_inputs() {
        assert_eq!(normalize_file_key("src/foo.rs"), Some("src/foo.rs".into()));
        assert_eq!(
            normalize_file_key("./src/foo.rs"),
            Some("src/foo.rs".into())
        );
        assert_eq!(
            normalize_file_key("src\\nested\\foo.rs"),
            Some("src/nested/foo.rs".into())
        );
        assert_eq!(normalize_file_key("src/foo.rs/"), Some("src/foo.rs".into()));
        assert_eq!(normalize_file_key(""), None);
        assert_eq!(normalize_file_key("."), None);
        assert_eq!(normalize_file_key(".."), None);
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
