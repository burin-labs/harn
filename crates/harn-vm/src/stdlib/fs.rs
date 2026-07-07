use crate::value::VmDictExt;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use std::{cell::RefCell, thread_local};

use crate::runtime_limits::RuntimeLimits;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::testbench::overlay_fs::helpers as overlay;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

mod find_text;

thread_local! {
    static FILE_TEXT_CACHE: RefCell<BTreeMap<PathBuf, FileTextCacheEntry>> = const { RefCell::new(BTreeMap::new()) };
}

const FILE_TEXT_CACHE_MAX_ENTRIES: usize = RuntimeLimits::DEFAULT.max_file_text_cache_entries;

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &READ_FILE_BUILTIN_DEF,
    &READ_FILE_RESULT_BUILTIN_DEF,
    &READ_FILE_BYTES_BUILTIN_DEF,
    &WRITE_FILE_BUILTIN_DEF,
    &WRITE_FILE_BYTES_BUILTIN_DEF,
    &FILE_EXISTS_BUILTIN_DEF,
    &PATH_STATUS_BUILTIN_DEF,
    &DELETE_FILE_BUILTIN_DEF,
    &APPEND_FILE_BUILTIN_DEF,
    &LIST_DIR_BUILTIN_DEF,
    &MKDIR_BUILTIN_DEF,
    &PATH_JOIN_BUILTIN_DEF,
    &COPY_FILE_BUILTIN_DEF,
    &TEMP_DIR_BUILTIN_DEF,
    &WORKSPACE_TEMP_DIR_BUILTIN_DEF,
    &MKDTEMP_BUILTIN_DEF,
    &MKDTEMP_IN_WORKSPACE_BUILTIN_DEF,
    &STAT_BUILTIN_DEF,
    &MOVE_FILE_BUILTIN_DEF,
    &READ_LINES_BUILTIN_DEF,
    &WALK_DIR_BUILTIN_DEF,
    &GLOB_BUILTIN_DEF,
    &find_text::FIND_TEXT_BUILTIN_DEF,
];

#[derive(Clone)]
struct FileTextCacheEntry {
    content: arcstr::ArcStr,
    len: u64,
    modified: Option<SystemTime>,
}

pub(crate) fn reset_fs_state() {
    FILE_TEXT_CACHE.with(|cache| cache.borrow_mut().clear());
}

#[derive(Clone, Copy)]
struct WalkDirOptions {
    max_depth: Option<usize>,
    follow_symlinks: bool,
    long_running: bool,
}

#[derive(Clone)]
struct WalkDirEntry {
    path: String,
    is_dir: bool,
    is_file: bool,
    depth: i64,
}

#[derive(Clone)]
struct GlobOptions {
    base: String,
    long_running: bool,
}

fn resolve_fs_path(path: &str) -> PathBuf {
    crate::stdlib::process::resolve_source_relative_path(path)
}

fn result_ok(value: VmValue) -> VmValue {
    VmValue::enum_variant("Result", "Ok", vec![value])
}

fn result_err(value: VmValue) -> VmValue {
    VmValue::enum_variant("Result", "Err", vec![value])
}

fn bool_option(opts: &crate::value::DictMap, key: &str) -> Option<bool> {
    match opts.get(key) {
        Some(VmValue::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn string_option(opts: &crate::value::DictMap, key: &str) -> Option<String> {
    match opts.get(key) {
        Some(VmValue::String(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn usize_option(opts: &crate::value::DictMap, key: &str) -> Option<usize> {
    opts.get(key)
        .and_then(VmValue::as_int)
        .and_then(|value| usize::try_from(value).ok())
}

fn u64_option(opts: &crate::value::DictMap, key: &str) -> Option<u64> {
    opts.get(key)
        .and_then(VmValue::as_int)
        .and_then(|value| u64::try_from(value).ok())
}

fn int_option(opts: &crate::value::DictMap, key: &str) -> Option<i64> {
    opts.get(key).and_then(VmValue::as_int)
}

fn string_list_option(opts: &crate::value::DictMap, key: &str) -> Vec<String> {
    match opts.get(key) {
        Some(VmValue::String(value)) => vec![value.to_string()],
        Some(VmValue::List(values)) => values
            .iter()
            .filter_map(|value| match value {
                VmValue::String(text) => Some(text.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn parse_walk_dir_options(args: &[VmValue]) -> WalkDirOptions {
    let mut options = WalkDirOptions {
        max_depth: None,
        follow_symlinks: false,
        long_running: false,
    };
    if let Some(VmValue::Dict(opts)) = args.get(1) {
        if let Some(v) = opts.get("max_depth").and_then(|v| v.as_int()) {
            if v >= 0 {
                options.max_depth = Some(v as usize);
            }
        }
        options.follow_symlinks = bool_option(opts, "follow_symlinks").unwrap_or(false);
        options.long_running = bool_option(opts, "long_running")
            .or_else(|| bool_option(opts, "background"))
            .unwrap_or(false);
    }
    options
}

fn walk_dir_entries(
    resolved: &PathBuf,
    options: WalkDirOptions,
    cancel: Option<&AtomicBool>,
) -> Vec<WalkDirEntry> {
    let mut walker = walkdir::WalkDir::new(resolved).follow_links(options.follow_symlinks);
    if let Some(d) = options.max_depth {
        walker = walker.max_depth(d);
    }
    let mut entries = Vec::new();
    for entry in walker.into_iter().filter_map(|e| e.ok()) {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            break;
        }
        let path = entry.path();
        entries.push(WalkDirEntry {
            path: path.to_string_lossy().replace('\\', "/"),
            is_dir: entry.file_type().is_dir(),
            is_file: entry.file_type().is_file(),
            depth: entry.depth() as i64,
        });
    }
    entries
}

fn walk_entry_to_vm(entry: WalkDirEntry) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.put_str("path", entry.path);
    dict.insert("is_dir".to_string(), VmValue::Bool(entry.is_dir));
    dict.insert("is_file".to_string(), VmValue::Bool(entry.is_file));
    dict.insert("depth".to_string(), VmValue::Int(entry.depth));
    VmValue::dict(dict)
}

fn walk_entries_to_json(entries: Vec<WalkDirEntry>) -> serde_json::Value {
    serde_json::Value::Array(
        entries
            .into_iter()
            .map(|entry| {
                serde_json::json!({
                    "path": entry.path,
                    "is_dir": entry.is_dir,
                    "is_file": entry.is_file,
                    "depth": entry.depth,
                })
            })
            .collect(),
    )
}

fn parse_glob_options(args: &[VmValue]) -> GlobOptions {
    let mut options = GlobOptions {
        base: ".".to_string(),
        long_running: false,
    };
    match args.get(1) {
        Some(VmValue::Dict(opts)) => {
            options.base = string_option(opts, "base").unwrap_or_else(|| ".".to_string());
            options.long_running = bool_option(opts, "long_running")
                .or_else(|| bool_option(opts, "background"))
                .unwrap_or(false);
        }
        Some(value) => {
            let base = value.display();
            if !base.is_empty() {
                options.base = base;
            }
            if let Some(VmValue::Dict(opts)) = args.get(2) {
                options.long_running = bool_option(opts, "long_running")
                    .or_else(|| bool_option(opts, "background"))
                    .unwrap_or(false);
            }
        }
        None => {}
    }
    options
}

fn glob_matches(
    pattern: &str,
    base: &PathBuf,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<String>, VmError> {
    let mut builder = globset::GlobSetBuilder::new();
    let glob = globset::Glob::new(pattern).map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!("glob: {e}"))))
    })?;
    builder.add(glob);
    let set = builder.build().map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!("glob: {e}"))))
    })?;
    let mut matches = Vec::new();
    for entry in walkdir::WalkDir::new(base)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            break;
        }
        let rel = match entry.path().strip_prefix(base) {
            Ok(p) => p.to_path_buf(),
            Err(_) => continue,
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if rel_str.is_empty() {
            continue;
        }
        if set.is_match(&rel_str) {
            matches.push(entry.path().to_string_lossy().replace('\\', "/"));
        }
    }
    matches.sort();
    Ok(matches)
}

fn metadata_signature(path: &PathBuf) -> Option<(u64, Option<SystemTime>)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.len(), metadata.modified().ok()))
}

fn read_cached_text(path: &PathBuf) -> Option<arcstr::ArcStr> {
    FILE_TEXT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let entry = cache.get(path).cloned()?;
        match metadata_signature(path) {
            Some((len, modified)) if len == entry.len && modified == entry.modified => {
                Some(entry.content)
            }
            _ => {
                cache.remove(path);
                None
            }
        }
    })
}

fn write_cached_text(path: PathBuf, content: arcstr::ArcStr) {
    let Some((len, modified)) = metadata_signature(&path) else {
        return;
    };
    FILE_TEXT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= FILE_TEXT_CACHE_MAX_ENTRIES && !cache.contains_key(&path) {
            cache.pop_first();
        }
        cache.insert(
            path,
            FileTextCacheEntry {
                content,
                len,
                modified,
            },
        );
    });
}

pub(crate) fn register_fs_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "read_file(path: string) -> string",
    category = "fs",
    doc = "Read an entire UTF-8 file or embedded stdlib prompt asset."
)]
fn read_file_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    if let Some(source) = crate::stdlib_modules::get_stdlib_prompt_asset(&path) {
        return Ok(VmValue::String(arcstr::ArcStr::from(source)));
    }
    if crate::stdlib::asset_paths::stdlib_prompt_asset_path(&path).is_some() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("Unknown stdlib prompt asset {path}"),
        ))));
    }
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "read_file",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    if let Some(cached) = read_cached_text(&resolved) {
        return Ok(VmValue::String(cached));
    }
    match overlay::read_to_string(&resolved) {
        Ok(content) => {
            let shared: arcstr::ArcStr = arcstr::ArcStr::from(content);
            write_cached_text(resolved.clone(), shared.clone());
            Ok(VmValue::String(shared))
        }
        Err(e) => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("Failed to read file {}: {e}", resolved.display()),
        )))),
    }
}

#[harn_builtin(
    sig = "read_file_result(path: string) -> dict",
    category = "fs",
    doc = "Read a UTF-8 file and return Result.Ok or Result.Err."
)]
fn read_file_result_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    if let Some(source) = crate::stdlib_modules::get_stdlib_prompt_asset(&path) {
        return Ok(result_ok(VmValue::String(arcstr::ArcStr::from(source))));
    }
    if crate::stdlib::asset_paths::stdlib_prompt_asset_path(&path).is_some() {
        return Ok(result_err(VmValue::String(arcstr::ArcStr::from(format!(
            "Unknown stdlib prompt asset {path}"
        )))));
    }
    let resolved = resolve_fs_path(&path);
    if let Err(error) = crate::stdlib::sandbox::enforce_fs_path(
        "read_file_result",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    ) {
        return Ok(result_err(VmValue::String(arcstr::ArcStr::from(
            error.to_string(),
        ))));
    }
    if let Some(cached) = read_cached_text(&resolved) {
        return Ok(result_ok(VmValue::String(cached)));
    }
    match overlay::read_to_string(&resolved) {
        Ok(content) => {
            let shared: arcstr::ArcStr = arcstr::ArcStr::from(content);
            write_cached_text(resolved.clone(), shared.clone());
            Ok(result_ok(VmValue::String(shared)))
        }
        Err(e) => Ok(result_err(VmValue::String(arcstr::ArcStr::from(format!(
            "Failed to read file {}: {e}",
            resolved.display()
        ))))),
    }
}

#[harn_builtin(
    sig = "read_file_bytes(path: string) -> bytes",
    category = "fs",
    doc = "Read an entire file as bytes."
)]
fn read_file_bytes_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "read_file_bytes",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    match overlay::read(&resolved) {
        Ok(content) => Ok(VmValue::Bytes(std::sync::Arc::new(content))),
        Err(e) => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("Failed to read file {}: {e}", resolved.display()),
        )))),
    }
}

#[harn_builtin(
    sig = "write_file(path: string, content: string) -> nil",
    category = "fs",
    doc = "Write UTF-8 text to a file."
)]
fn write_file_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 2 {
        let path = args[0].display();
        let content = args[1].display();
        let resolved = resolve_fs_path(&path);
        crate::stdlib::sandbox::enforce_fs_path(
            "write_file",
            &resolved,
            crate::stdlib::sandbox::FsAccess::Write,
        )?;
        overlay::write_scoped("write_file", &resolved, content.as_bytes()).map_err(|e| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "Failed to write file {}: {e}",
                resolved.display()
            ))))
        })?;
        let bytes = content.len();
        write_cached_text(resolved.clone(), arcstr::ArcStr::from(content));
        queue_file_edited_for(&resolved, "write", bytes);
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "write_file_bytes(path: string, content: bytes) -> nil",
    category = "fs",
    doc = "Write bytes to a file."
)]
fn write_file_bytes_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 2 {
        let path = args[0].display();
        let resolved = resolve_fs_path(&path);
        let content = match &args[1] {
            VmValue::Bytes(bytes) => bytes.as_slice(),
            other => {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!(
                        "write_file_bytes expects bytes content, got {}",
                        other.type_name()
                    ),
                ))));
            }
        };
        let len = content.len();
        crate::stdlib::sandbox::enforce_fs_path(
            "write_file_bytes",
            &resolved,
            crate::stdlib::sandbox::FsAccess::Write,
        )?;
        overlay::write_scoped("write_file_bytes", &resolved, content).map_err(|e| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "Failed to write file {}: {e}",
                resolved.display()
            ))))
        })?;
        FILE_TEXT_CACHE.with(|cache| {
            cache.borrow_mut().remove(&resolved);
        });
        queue_file_edited_for(&resolved, "write", len);
    }
    Ok(VmValue::Nil)
}

fn queue_file_edited_for(resolved: &std::path::Path, operation: &str, bytes: usize) {
    crate::orchestration::queue_file_edited(
        &resolved.to_string_lossy(),
        serde_json::json!({"operation": operation, "bytes": bytes}),
    );
}

fn edited_byte_count(bytes: u64) -> usize {
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

#[harn_builtin(
    sig = "file_exists(path: string) -> bool",
    category = "fs",
    doc = "Return whether a file-system path exists."
)]
fn file_exists_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let resolved = resolve_fs_path(&path);
    // A presence probe for a path outside the sandbox reads as "absent",
    // matching how OS sandboxes behave (paths outside the jail appear
    // non-existent) rather than crashing the pipeline with a violation. Only
    // a fully out-of-scope path (not a read-only-root denial) maps to false;
    // reads inside a read-only root are already permitted by the scope check.
    // Reading file *content* outside roots still errors (see read_text).
    if let Err(violation) = crate::stdlib::sandbox::check_fs_path_scope(
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    ) {
        if !violation.read_only {
            return Ok(VmValue::Bool(false));
        }
        return Err(crate::stdlib::sandbox::sandbox_rejection(
            violation.message("file_exists"),
        ));
    }
    Ok(VmValue::Bool(overlay::exists(&resolved)))
}

fn fs_access_label(access: crate::stdlib::sandbox::FsAccess) -> &'static str {
    match access {
        crate::stdlib::sandbox::FsAccess::Read => "read",
        crate::stdlib::sandbox::FsAccess::Write => "write",
        crate::stdlib::sandbox::FsAccess::Delete => "delete",
    }
}

fn parse_path_status_access(args: &[VmValue]) -> Result<crate::stdlib::sandbox::FsAccess, VmError> {
    let Some(raw) = args.get(1) else {
        return Ok(crate::stdlib::sandbox::FsAccess::Read);
    };
    let text = match raw {
        VmValue::Nil => return Ok(crate::stdlib::sandbox::FsAccess::Read),
        VmValue::String(text) => text.as_str(),
        other => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                    "path_status access must be a string, got {}",
                    other.type_name()
                ),
            ))));
        }
    };
    match text {
        "" | "read" => Ok(crate::stdlib::sandbox::FsAccess::Read),
        "write" => Ok(crate::stdlib::sandbox::FsAccess::Write),
        "delete" | "remove" => Ok(crate::stdlib::sandbox::FsAccess::Delete),
        _ => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "path_status access must be one of: read, write, delete",
        )))),
    }
}

fn base_path_status(
    path: String,
    resolved: &PathBuf,
    access: crate::stdlib::sandbox::FsAccess,
) -> BTreeMap<String, VmValue> {
    let mut info = BTreeMap::new();
    info.put_str("path", path);
    info.put_str("resolved_path", resolved.to_string_lossy().into_owned());
    info.put_str("access", fs_access_label(access));
    info
}

fn path_status_from_metadata(
    path: String,
    resolved: &PathBuf,
    access: crate::stdlib::sandbox::FsAccess,
    metadata: std::fs::Metadata,
) -> VmValue {
    let status = if metadata.is_file() {
        "present_file"
    } else if metadata.is_dir() {
        "present_dir"
    } else {
        "present_other"
    };
    let kind = if metadata.is_file() {
        "file"
    } else if metadata.is_dir() {
        "dir"
    } else {
        "other"
    };
    let mut info = base_path_status(path, resolved, access);
    info.put_str("status", status);
    info.put_str("kind", kind);
    info.insert("visible".to_string(), VmValue::Bool(true));
    info.insert("exists".to_string(), VmValue::Bool(true));
    info.insert("size".to_string(), VmValue::Int(metadata.len() as i64));
    info.insert("is_file".to_string(), VmValue::Bool(metadata.is_file()));
    info.insert("is_dir".to_string(), VmValue::Bool(metadata.is_dir()));
    info.insert(
        "readonly".to_string(),
        VmValue::Bool(metadata.permissions().readonly()),
    );
    if let Ok(modified) = metadata.modified() {
        if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
            info.insert("modified".to_string(), VmValue::Float(dur.as_secs_f64()));
        }
    }
    VmValue::dict(info)
}

fn missing_path_status(
    path: String,
    resolved: &PathBuf,
    access: crate::stdlib::sandbox::FsAccess,
) -> VmValue {
    let mut info = base_path_status(path, resolved, access);
    info.put_str("status", "missing");
    info.put_str("kind", "missing");
    info.insert("visible".to_string(), VmValue::Bool(true));
    info.insert("exists".to_string(), VmValue::Bool(false));
    VmValue::dict(info)
}

fn scope_denied_path_status(
    path: String,
    resolved: &PathBuf,
    access: crate::stdlib::sandbox::FsAccess,
    violation: crate::stdlib::sandbox::SandboxViolation,
) -> VmValue {
    let status = if violation.read_only {
        "read_only_denied"
    } else {
        "scope_denied"
    };
    let mut info = base_path_status(path, resolved, access);
    info.put_str("status", status);
    info.put_str("kind", status);
    info.insert("visible".to_string(), VmValue::Bool(false));
    info.insert("exists".to_string(), VmValue::Nil);
    info.insert("read_only".to_string(), VmValue::Bool(violation.read_only));
    info.put_str(
        "attempted",
        violation.attempted.to_string_lossy().into_owned(),
    );
    info.insert(
        "roots".to_string(),
        VmValue::List(
            violation
                .roots
                .iter()
                .map(|root| {
                    VmValue::String(arcstr::ArcStr::from(root.to_string_lossy().into_owned()))
                })
                .collect(),
        ),
    );
    info.put_str("message", violation.message("path_status"));
    VmValue::dict(info)
}

fn stat_error_path_status(
    path: String,
    resolved: &PathBuf,
    access: crate::stdlib::sandbox::FsAccess,
    error: std::io::Error,
) -> VmValue {
    let mut info = base_path_status(path, resolved, access);
    info.put_str("status", "stat_error");
    info.put_str("kind", "stat_error");
    info.insert("visible".to_string(), VmValue::Bool(false));
    info.insert("exists".to_string(), VmValue::Nil);
    info.put_str("error_kind", format!("{:?}", error.kind()));
    info.put_str(
        "message",
        format!("Failed to stat {}: {error}", resolved.display()),
    );
    VmValue::dict(info)
}

#[harn_builtin(
    sig = "path_status(path: string, access?: string) -> dict",
    category = "fs",
    doc = "Return structured filesystem visibility status without collapsing scope denial into absence."
)]
fn path_status_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let access = parse_path_status_access(args)?;
    let resolved = resolve_fs_path(&path);
    if let Err(violation) = crate::stdlib::sandbox::check_fs_path_scope(&resolved, access) {
        return Ok(scope_denied_path_status(path, &resolved, access, violation));
    }
    match std::fs::metadata(&resolved) {
        Ok(metadata) => Ok(path_status_from_metadata(path, &resolved, access, metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(missing_path_status(path, &resolved, access))
        }
        Err(error) => Ok(stat_error_path_status(path, &resolved, access, error)),
    }
}

#[harn_builtin(
    sig = "delete_file(path: string) -> nil",
    category = "fs",
    doc = "Delete a file or directory."
)]
fn delete_file_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "delete_file",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Delete,
    )?;
    // Overlay treats files and directories alike by recording an overlay deletion marker.
    if crate::testbench::overlay_fs::active_overlay().is_some() {
        overlay::remove_file(&resolved).map_err(|e| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "Failed to delete {}: {e}",
                resolved.display()
            ))))
        })?;
    } else if resolved.is_dir() {
        std::fs::remove_dir_all(&resolved).map_err(|e| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "Failed to delete directory {}: {e}",
                resolved.display()
            ))))
        })?;
    } else {
        std::fs::remove_file(&resolved).map_err(|e| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "Failed to delete file {}: {e}",
                resolved.display()
            ))))
        })?;
    }
    FILE_TEXT_CACHE.with(|cache| {
        cache.borrow_mut().remove(&resolved);
    });
    queue_file_edited_for(&resolved, "delete", 0);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "append_file(path: string, content: string) -> nil",
    category = "fs",
    doc = "Append UTF-8 text to a file."
)]
fn append_file_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 2 {
        let path = args[0].display();
        let content = args[1].display();
        let resolved = resolve_fs_path(&path);
        crate::stdlib::sandbox::enforce_fs_path(
            "append_file",
            &resolved,
            crate::stdlib::sandbox::FsAccess::Write,
        )?;
        overlay::append_scoped("append_file", &resolved, content.as_bytes()).map_err(|e| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "Failed to append to file {}: {e}",
                resolved.display()
            ))))
        })?;
        let bytes = content.len();
        FILE_TEXT_CACHE.with(|cache| {
            cache.borrow_mut().remove(&resolved);
        });
        queue_file_edited_for(&resolved, "append", bytes);
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "list_dir(path?: string) -> list",
    category = "fs",
    doc = "Return sorted directory entry names."
)]
fn list_dir_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args
        .first()
        .map(|a| a.display())
        .unwrap_or_else(|| ".".to_string());
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "list_dir",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    let entries = overlay::read_dir(&resolved).map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "Failed to list directory {}: {e}",
            resolved.display()
        ))))
    })?;
    let mut result = Vec::new();
    for entry in entries {
        let Some(name) = entry.path.file_name() else {
            continue;
        };
        let name = name.to_string_lossy().into_owned();
        result.push(VmValue::String(arcstr::ArcStr::from(name)));
    }
    result.sort_by_key(|a| a.display());
    Ok(VmValue::List(std::sync::Arc::new(result)))
}

#[harn_builtin(
    sig = "mkdir(path: string, recursive?: bool) -> nil",
    category = "fs",
    doc = "Create a directory. By default missing parents are created; pass recursive=false for single-directory creation that fails if the target already exists."
)]
fn mkdir_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let recursive = !matches!(args.get(1), Some(VmValue::Bool(false)));
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "mkdir",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Write,
    )?;
    let result = overlay::create_dir_scoped("mkdir", &resolved, recursive);
    result.map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "Failed to create directory {}: {e}",
            resolved.display()
        ))))
    })?;
    queue_file_edited_for(&resolved, "mkdir", 0);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "path_join(...args: any) -> string",
    category = "fs",
    doc = "Join path segments with the platform path separator."
)]
fn path_join_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut path = std::path::PathBuf::new();
    for arg in args {
        path.push(arg.display());
    }
    Ok(VmValue::String(arcstr::ArcStr::from(
        path.to_string_lossy().into_owned().as_str(),
    )))
}

#[harn_builtin(
    sig = "copy_file(src: string, dst: string) -> nil",
    category = "fs",
    doc = "Copy a file to a destination path."
)]
fn copy_file_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 2 {
        let src = args[0].display();
        let dst = args[1].display();
        let resolved_src = resolve_fs_path(&src);
        let resolved_dst = resolve_fs_path(&dst);
        crate::stdlib::sandbox::enforce_fs_path(
            "copy_file",
            &resolved_src,
            crate::stdlib::sandbox::FsAccess::Read,
        )?;
        crate::stdlib::sandbox::enforce_fs_path(
            "copy_file",
            &resolved_dst,
            crate::stdlib::sandbox::FsAccess::Write,
        )?;
        let bytes =
            overlay::copy_scoped("copy_file", &resolved_src, &resolved_dst).map_err(|e| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                    "Failed to copy {} to {}: {e}",
                    resolved_src.display(),
                    resolved_dst.display()
                ))))
            })?;
        FILE_TEXT_CACHE.with(|cache| {
            cache.borrow_mut().remove(&resolved_dst);
        });
        queue_file_edited_for(&resolved_dst, "copy", edited_byte_count(bytes));
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "temp_dir() -> string",
    category = "fs",
    doc = "Return the host temporary directory path."
)]
fn temp_dir_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::String(arcstr::ArcStr::from(
        std::env::temp_dir().to_string_lossy().into_owned().as_str(),
    )))
}

#[harn_builtin(
    sig = "workspace_temp_dir() -> string",
    category = "fs",
    doc = "Return a sandbox-writable workspace-local temporary directory path, creating it lazily."
)]
fn workspace_temp_dir_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::String(arcstr::ArcStr::from(
        workspace_temp_root()?.to_string_lossy().into_owned(),
    )))
}

#[harn_builtin(
    sig = "mkdtemp(prefix?: string) -> string",
    category = "fs",
    doc = "Create a new uniquely-named directory under the host temp dir and return its absolute path. The caller owns the directory lifecycle."
)]
fn mkdtemp_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dir_name = unique_temp_dir_name(args, "harn-");
    let path = std::env::temp_dir().join(dir_name);
    crate::stdlib::sandbox::enforce_fs_path(
        "mkdtemp",
        &path,
        crate::stdlib::sandbox::FsAccess::Write,
    )?;
    std::fs::create_dir(&path).map_err(|error| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "mkdtemp: failed to create {}: {error}",
            path.display()
        ))))
    })?;
    Ok(VmValue::String(arcstr::ArcStr::from(
        path.to_string_lossy().into_owned(),
    )))
}

#[harn_builtin(
    sig = "mkdtemp_in_workspace(prefix?: string) -> string",
    category = "fs",
    doc = "Create a new uniquely-named directory under workspace_temp_dir() and return its path."
)]
fn mkdtemp_in_workspace_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let root = workspace_temp_root()?;
    let path = root.join(unique_temp_dir_name(args, "harn-"));
    crate::stdlib::sandbox::enforce_fs_path(
        "mkdtemp_in_workspace",
        &path,
        crate::stdlib::sandbox::FsAccess::Write,
    )?;
    std::fs::create_dir(&path).map_err(|error| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "mkdtemp_in_workspace: failed to create {}: {error}",
            path.display()
        ))))
    })?;
    queue_file_edited_for(&path, "mkdtemp_in_workspace", 0);
    Ok(VmValue::String(arcstr::ArcStr::from(
        path.to_string_lossy().into_owned(),
    )))
}

fn workspace_temp_root() -> Result<PathBuf, VmError> {
    if let Some(policy) = crate::orchestration::current_execution_policy() {
        if !matches!(
            policy.sandbox_profile,
            crate::orchestration::SandboxProfile::Unrestricted
        ) {
            if let Some(path) = crate::stdlib::sandbox::workspace_local_tmpdir(&policy) {
                return Ok(path);
            }
        }
    }
    let path = resolve_fs_path(crate::stdlib::sandbox::WORKSPACE_TMPDIR_NAME);
    crate::stdlib::sandbox::enforce_fs_path(
        "workspace_temp_dir",
        &path,
        crate::stdlib::sandbox::FsAccess::Write,
    )?;
    std::fs::create_dir_all(&path).map_err(|error| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "workspace_temp_dir: failed to create {}: {error}",
            path.display()
        ))))
    })?;
    let ignore = path.join(".gitignore");
    if !ignore.exists() {
        let _ = std::fs::write(&ignore, "# Created by Harn; safe to delete.\n*\n");
    }
    Ok(path)
}

fn unique_temp_dir_name(args: &[VmValue], default_prefix: &str) -> String {
    let raw_prefix = args
        .first()
        .map(|v| v.display())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_prefix.to_string());
    let sanitized_prefix: String = raw_prefix
        .chars()
        .filter(|c| !matches!(c, '/' | '\\'))
        .collect();
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let short_suffix = &suffix[suffix.len().saturating_sub(12)..];
    format!("{sanitized_prefix}{short_suffix}")
}

#[harn_builtin(
    sig = "stat(path: string) -> dict",
    category = "fs",
    doc = "Return metadata for a file-system path."
)]
fn stat_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "stat",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    let metadata = std::fs::metadata(&resolved).map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "Failed to stat {}: {e}",
            resolved.display()
        ))))
    })?;
    let mut info = BTreeMap::new();
    info.insert("size".to_string(), VmValue::Int(metadata.len() as i64));
    info.insert("is_file".to_string(), VmValue::Bool(metadata.is_file()));
    info.insert("is_dir".to_string(), VmValue::Bool(metadata.is_dir()));
    info.insert(
        "readonly".to_string(),
        VmValue::Bool(metadata.permissions().readonly()),
    );
    if let Ok(modified) = metadata.modified() {
        if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
            info.insert("modified".to_string(), VmValue::Float(dur.as_secs_f64()));
        }
    }
    Ok(VmValue::dict(info))
}

#[harn_builtin(
    sig = "move_file(src: string, dst: string) -> nil",
    category = "fs",
    doc = "Move a file to a destination path."
)]
fn move_file_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "move_file: src and dst are required",
        ))));
    }
    let src = resolve_fs_path(&args[0].display());
    let dst = resolve_fs_path(&args[1].display());
    crate::stdlib::sandbox::enforce_fs_path(
        "move_file",
        &src,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    crate::stdlib::sandbox::enforce_fs_path(
        "move_file",
        &src,
        crate::stdlib::sandbox::FsAccess::Delete,
    )?;
    crate::stdlib::sandbox::enforce_fs_path(
        "move_file",
        &dst,
        crate::stdlib::sandbox::FsAccess::Write,
    )?;
    if let Ok(bytes) = overlay::rename_scoped("move_file", &src, &dst) {
        FILE_TEXT_CACHE.with(|c| {
            let mut c = c.borrow_mut();
            c.remove(&src);
            c.remove(&dst);
        });
        queue_file_edited_for(&src, "move_from", 0);
        queue_file_edited_for(&dst, "move", edited_byte_count(bytes));
        return Ok(VmValue::Nil);
    }
    let bytes = overlay::copy_scoped("move_file", &src, &dst).map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "move_file: copy failed: {e}"
        ))))
    })?;
    overlay::remove_file(&src).map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "move_file: remove src failed: {e}"
        ))))
    })?;
    FILE_TEXT_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.remove(&src);
        c.remove(&dst);
    });
    queue_file_edited_for(&src, "move_from", 0);
    queue_file_edited_for(&dst, "move", edited_byte_count(bytes));
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "read_lines(path: string) -> list",
    category = "fs",
    doc = "Read a UTF-8 file as a list of lines."
)]
fn read_lines_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "read_lines",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    let content = overlay::read_to_string(&resolved).map_err(|e| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "read_lines: {}: {e}",
            resolved.display()
        ))))
    })?;
    let lines: Vec<VmValue> = content
        .lines()
        .map(|l| VmValue::String(arcstr::ArcStr::from(l)))
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(lines)))
}

#[harn_builtin(
    sig = "walk_dir(path: string, options?: dict) -> list",
    category = "fs",
    doc = "Recursively list files and directories."
)]
fn walk_dir_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let root = args.first().map(|a| a.display()).unwrap_or_default();
    if root.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "walk_dir: root path is required",
        ))));
    }
    let resolved = resolve_fs_path(&root);
    crate::stdlib::sandbox::enforce_fs_path(
        "walk_dir",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    let options = parse_walk_dir_options(args);
    if options.long_running {
        let session_id = crate::llm::current_agent_session_id().unwrap_or_default();
        let descriptor = format!("walk_dir {}", resolved.display());
        let handle = crate::stdlib::long_running::spawn_json_operation(
            "walk_dir",
            descriptor,
            session_id,
            move |cancel| {
                Ok(walk_entries_to_json(walk_dir_entries(
                    &resolved,
                    options,
                    Some(&cancel),
                )))
            },
        )
        .map_err(VmError::Runtime)?;
        return Ok(handle.into_vm_value());
    }
    let entries = walk_dir_entries(&resolved, options, None)
        .into_iter()
        .map(walk_entry_to_vm)
        .collect::<Vec<_>>();
    Ok(VmValue::List(std::sync::Arc::new(entries)))
}

#[harn_builtin(
    sig = "glob(pattern: string, base_or_options?: any, options?: dict) -> list",
    category = "fs",
    doc = "Match files under a base directory using a glob pattern."
)]
fn glob_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let pattern = args.first().map(|a| a.display()).unwrap_or_default();
    if pattern.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "glob: pattern is required",
        ))));
    }
    let options = parse_glob_options(args);
    let base = resolve_fs_path(&options.base);
    crate::stdlib::sandbox::enforce_fs_path("glob", &base, crate::stdlib::sandbox::FsAccess::Read)?;
    if options.long_running {
        let session_id = crate::llm::current_agent_session_id().unwrap_or_default();
        let descriptor = format!("glob {} in {}", pattern, base.display());
        let handle = crate::stdlib::long_running::spawn_json_operation(
            "glob",
            descriptor,
            session_id,
            move |cancel| {
                glob_matches(&pattern, &base, Some(&cancel))
                    .map(|items| {
                        serde_json::Value::Array(
                            items.into_iter().map(serde_json::Value::String).collect(),
                        )
                    })
                    .map_err(|error| error.to_string())
            },
        )
        .map_err(VmError::Runtime)?;
        return Ok(handle.into_vm_value());
    }
    let matches = glob_matches(&pattern, &base, None)?
        .into_iter()
        .map(|path| VmValue::String(arcstr::ArcStr::from(path)))
        .collect::<Vec<_>>();
    Ok(VmValue::List(std::sync::Arc::new(matches)))
}

#[cfg(test)]
mod tests;
