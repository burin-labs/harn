use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use std::{cell::RefCell, thread_local};

use crate::runtime_limits::RuntimeLimits;
use crate::stdlib::registration::{register_builtin_group, BuiltinGroup, SyncBuiltin};
use crate::testbench::overlay_fs::helpers as overlay;
use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity};

thread_local! {
    static FILE_TEXT_CACHE: RefCell<BTreeMap<PathBuf, FileTextCacheEntry>> = const { RefCell::new(BTreeMap::new()) };
}

const FILE_TEXT_CACHE_MAX_ENTRIES: usize = RuntimeLimits::DEFAULT.max_file_text_cache_entries;

const FS_SYNC_PRIMITIVES: &[SyncBuiltin] = &[
    SyncBuiltin::new("read_file", read_file_builtin)
        .signature("read_file(path)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Read an entire UTF-8 file or embedded stdlib prompt asset."),
    SyncBuiltin::new("read_file_result", read_file_result_builtin)
        .signature("read_file_result(path)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Read a UTF-8 file and return Result.Ok or Result.Err."),
    SyncBuiltin::new("read_file_bytes", read_file_bytes_builtin)
        .signature("read_file_bytes(path)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Read an entire file as bytes."),
    SyncBuiltin::new("write_file", write_file_builtin)
        .signature("write_file(path, content)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Write UTF-8 text to a file."),
    SyncBuiltin::new("write_file_bytes", write_file_bytes_builtin)
        .signature("write_file_bytes(path, content)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Write bytes to a file."),
    SyncBuiltin::new("file_exists", file_exists_builtin)
        .signature("file_exists(path)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Return whether a file-system path exists."),
    SyncBuiltin::new("delete_file", delete_file_builtin)
        .signature("delete_file(path)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Delete a file or directory."),
    SyncBuiltin::new("append_file", append_file_builtin)
        .signature("append_file(path, content)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Append UTF-8 text to a file."),
    SyncBuiltin::new("list_dir", list_dir_builtin)
        .signature("list_dir(path?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 1 })
        .doc("Return sorted directory entry names."),
    SyncBuiltin::new("mkdir", mkdir_builtin)
        .signature("mkdir(path)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Create a directory and any missing parents."),
    SyncBuiltin::new("path_join", path_join_builtin)
        .signature("path_join(args...)")
        .arity(VmBuiltinArity::Variadic)
        .doc("Join path segments with the platform path separator."),
    SyncBuiltin::new("copy_file", copy_file_builtin)
        .signature("copy_file(src, dst)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Copy a file to a destination path."),
    SyncBuiltin::new("temp_dir", temp_dir_builtin)
        .signature("temp_dir()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Return the host temporary directory path."),
    SyncBuiltin::new("stat", stat_builtin)
        .signature("stat(path)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Return metadata for a file-system path."),
    SyncBuiltin::new("move_file", move_file_builtin)
        .signature("move_file(src, dst)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Move a file to a destination path."),
    SyncBuiltin::new("read_lines", read_lines_builtin)
        .signature("read_lines(path)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Read a UTF-8 file as a list of lines."),
    SyncBuiltin::new("walk_dir", walk_dir_builtin)
        .signature("walk_dir(path, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Recursively list files and directories."),
    SyncBuiltin::new("glob", glob_builtin)
        .signature("glob(pattern, base_or_options?, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 3 })
        .doc("Match files under a base directory using a glob pattern."),
];

const FS_PRIMITIVES: BuiltinGroup<'static> =
    BuiltinGroup::new().category("fs").sync(FS_SYNC_PRIMITIVES);

#[derive(Clone)]
struct FileTextCacheEntry {
    content: Rc<str>,
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

fn bool_option(opts: &BTreeMap<String, VmValue>, key: &str) -> Option<bool> {
    match opts.get(key) {
        Some(VmValue::Bool(value)) => Some(*value),
        _ => None,
    }
}

fn string_option(opts: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    match opts.get(key) {
        Some(VmValue::String(value)) => Some(value.to_string()),
        _ => None,
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
    dict.insert("path".to_string(), VmValue::String(Rc::from(entry.path)));
    dict.insert("is_dir".to_string(), VmValue::Bool(entry.is_dir));
    dict.insert("is_file".to_string(), VmValue::Bool(entry.is_file));
    dict.insert("depth".to_string(), VmValue::Int(entry.depth));
    VmValue::Dict(Rc::new(dict))
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
    let glob = globset::Glob::new(pattern)
        .map_err(|e| VmError::Thrown(VmValue::String(Rc::from(format!("glob: {e}")))))?;
    builder.add(glob);
    let set = builder
        .build()
        .map_err(|e| VmError::Thrown(VmValue::String(Rc::from(format!("glob: {e}")))))?;
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

fn read_cached_text(path: &PathBuf) -> Option<Rc<str>> {
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

fn write_cached_text(path: PathBuf, content: Rc<str>) {
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
    register_builtin_group(vm, FS_PRIMITIVES);
}

fn read_file_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    if let Some(source) = crate::stdlib_modules::get_stdlib_prompt_asset(&path) {
        return Ok(VmValue::String(Rc::from(source)));
    }
    if crate::stdlib::asset_paths::stdlib_prompt_asset_path(&path).is_some() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "Unknown stdlib prompt asset {path}"
        )))));
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
            let shared: Rc<str> = Rc::from(content);
            write_cached_text(resolved.clone(), shared.clone());
            Ok(VmValue::String(shared))
        }
        Err(e) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "Failed to read file {}: {e}",
            resolved.display()
        ))))),
    }
}

fn read_file_result_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    if let Some(source) = crate::stdlib_modules::get_stdlib_prompt_asset(&path) {
        return Ok(result_ok(VmValue::String(Rc::from(source))));
    }
    if crate::stdlib::asset_paths::stdlib_prompt_asset_path(&path).is_some() {
        return Ok(result_err(VmValue::String(Rc::from(format!(
            "Unknown stdlib prompt asset {path}"
        )))));
    }
    let resolved = resolve_fs_path(&path);
    if let Err(error) = crate::stdlib::sandbox::enforce_fs_path(
        "read_file_result",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    ) {
        return Ok(result_err(VmValue::String(Rc::from(error.to_string()))));
    }
    if let Some(cached) = read_cached_text(&resolved) {
        return Ok(result_ok(VmValue::String(cached)));
    }
    match overlay::read_to_string(&resolved) {
        Ok(content) => {
            let shared: Rc<str> = Rc::from(content);
            write_cached_text(resolved.clone(), shared.clone());
            Ok(result_ok(VmValue::String(shared)))
        }
        Err(e) => Ok(result_err(VmValue::String(Rc::from(format!(
            "Failed to read file {}: {e}",
            resolved.display()
        ))))),
    }
}

fn read_file_bytes_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "read_file_bytes",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    match overlay::read(&resolved) {
        Ok(content) => Ok(VmValue::Bytes(Rc::new(content))),
        Err(e) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "Failed to read file {}: {e}",
            resolved.display()
        ))))),
    }
}

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
        overlay::write(&resolved, content.as_bytes()).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to write file {}: {e}",
                resolved.display()
            ))))
        })?;
        let bytes = content.len();
        write_cached_text(resolved.clone(), Rc::from(content));
        queue_file_edited_for(&resolved, "write", bytes);
    }
    Ok(VmValue::Nil)
}

fn write_file_bytes_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 2 {
        let path = args[0].display();
        let resolved = resolve_fs_path(&path);
        let content = match &args[1] {
            VmValue::Bytes(bytes) => bytes.as_slice(),
            other => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "write_file_bytes expects bytes content, got {}",
                    other.type_name()
                )))));
            }
        };
        let len = content.len();
        crate::stdlib::sandbox::enforce_fs_path(
            "write_file_bytes",
            &resolved,
            crate::stdlib::sandbox::FsAccess::Write,
        )?;
        overlay::write(&resolved, content).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
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

fn file_exists_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "file_exists",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    Ok(VmValue::Bool(overlay::exists(&resolved)))
}

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
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to delete {}: {e}",
                resolved.display()
            ))))
        })?;
    } else if resolved.is_dir() {
        std::fs::remove_dir_all(&resolved).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to delete directory {}: {e}",
                resolved.display()
            ))))
        })?;
    } else {
        std::fs::remove_file(&resolved).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to delete file {}: {e}",
                resolved.display()
            ))))
        })?;
    }
    FILE_TEXT_CACHE.with(|cache| {
        cache.borrow_mut().remove(&resolved);
    });
    Ok(VmValue::Nil)
}

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
        overlay::append(&resolved, content.as_bytes()).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
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
        VmError::Thrown(VmValue::String(Rc::from(format!(
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
        result.push(VmValue::String(Rc::from(name)));
    }
    result.sort_by_key(|a| a.display());
    Ok(VmValue::List(Rc::new(result)))
}

fn mkdir_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "mkdir",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Write,
    )?;
    overlay::create_dir_all(&resolved).map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "Failed to create directory {}: {e}",
            resolved.display()
        ))))
    })?;
    Ok(VmValue::Nil)
}

fn path_join_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut path = std::path::PathBuf::new();
    for arg in args {
        path.push(arg.display());
    }
    Ok(VmValue::String(Rc::from(
        path.to_string_lossy().into_owned().as_str(),
    )))
}

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
        std::fs::copy(&resolved_src, &resolved_dst).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to copy {} to {}: {e}",
                resolved_src.display(),
                resolved_dst.display()
            ))))
        })?;
        FILE_TEXT_CACHE.with(|cache| {
            cache.borrow_mut().remove(&resolved_dst);
        });
    }
    Ok(VmValue::Nil)
}

fn temp_dir_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::String(Rc::from(
        std::env::temp_dir().to_string_lossy().into_owned().as_str(),
    )))
}

fn stat_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "stat",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    let metadata = std::fs::metadata(&resolved).map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
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
    Ok(VmValue::Dict(Rc::new(info)))
}

fn move_file_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Thrown(VmValue::String(Rc::from(
            "move_file: src and dst are required",
        ))));
    }
    let src = resolve_fs_path(&args[0].display());
    let dst = resolve_fs_path(&args[1].display());
    crate::stdlib::sandbox::enforce_fs_path(
        "move_file",
        &src,
        crate::stdlib::sandbox::FsAccess::Write,
    )?;
    crate::stdlib::sandbox::enforce_fs_path(
        "move_file",
        &dst,
        crate::stdlib::sandbox::FsAccess::Write,
    )?;
    if std::fs::rename(&src, &dst).is_ok() {
        FILE_TEXT_CACHE.with(|c| {
            let mut c = c.borrow_mut();
            c.remove(&src);
            c.remove(&dst);
        });
        return Ok(VmValue::Nil);
    }
    std::fs::copy(&src, &dst).map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "move_file: copy failed: {e}"
        ))))
    })?;
    std::fs::remove_file(&src).map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "move_file: remove src failed: {e}"
        ))))
    })?;
    FILE_TEXT_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.remove(&src);
        c.remove(&dst);
    });
    Ok(VmValue::Nil)
}

fn read_lines_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(|a| a.display()).unwrap_or_default();
    let resolved = resolve_fs_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "read_lines",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    let content = overlay::read_to_string(&resolved).map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "read_lines: {}: {e}",
            resolved.display()
        ))))
    })?;
    let lines: Vec<VmValue> = content
        .lines()
        .map(|l| VmValue::String(Rc::from(l)))
        .collect();
    Ok(VmValue::List(Rc::new(lines)))
}

fn walk_dir_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let root = args.first().map(|a| a.display()).unwrap_or_default();
    if root.is_empty() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(
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
    Ok(VmValue::List(Rc::new(entries)))
}

fn glob_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let pattern = args.first().map(|a| a.display()).unwrap_or_default();
    if pattern.is_empty() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(
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
        .map(|path| VmValue::String(Rc::from(path)))
        .collect::<Vec<_>>();
    Ok(VmValue::List(Rc::new(matches)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{LazyLock, Mutex};

    static LONG_RUNNING_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn vm() -> Vm {
        let mut vm = Vm::new();
        register_fs_builtins(&mut vm);
        vm
    }

    fn call(vm: &mut Vm, name: &str, args: Vec<VmValue>) -> Result<VmValue, VmError> {
        let f = vm.builtins.get(name).unwrap().clone();
        let mut out = String::new();
        f(&args, &mut out)
    }

    fn s(v: &str) -> VmValue {
        VmValue::String(Rc::from(v))
    }

    fn b(v: bool) -> VmValue {
        VmValue::Bool(v)
    }

    fn dict(entries: Vec<(&str, VmValue)>) -> VmValue {
        VmValue::Dict(Rc::new(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        ))
    }

    fn drain_feedback(handle_id: &str) -> serde_json::Value {
        // Cap total wait at 10s; each iteration parks on the inbox's
        // sync waiter so we wake the moment a background thread pushes
        // instead of polling. The outer loop accounts for items that
        // arrive for *other* handles (we requeue them via the
        // `seen_handles` log; long-running fs/glob ops push under the
        // empty session id).
        let overall_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen_handles = Vec::new();
        loop {
            for entry in crate::orchestration::agent_inbox::drain("") {
                assert_eq!(entry.kind, "tool_result");
                let payload: serde_json::Value = serde_json::from_str(&entry.content).unwrap();
                if payload["handle_id"] == handle_id {
                    return payload;
                }
                if let Some(seen) = payload["handle_id"].as_str() {
                    seen_handles.push(seen.to_string());
                }
            }
            let remaining = overall_deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                panic!(
                    "timed out waiting for feedback for {handle_id}; saw handles {seen_handles:?}"
                );
            }
            // Block until a producer notifies a push, or we hit the
            // remaining deadline. `wait_sync` returns `false` only
            // when nothing matched within the window; we still loop
            // once more to drain any stragglers that arrived just
            // before the timeout.
            if !crate::orchestration::agent_inbox::wait_sync("", remaining) {
                for entry in crate::orchestration::agent_inbox::drain("") {
                    assert_eq!(entry.kind, "tool_result");
                    let payload: serde_json::Value = serde_json::from_str(&entry.content).unwrap();
                    if payload["handle_id"] == handle_id {
                        return payload;
                    }
                    if let Some(seen) = payload["handle_id"].as_str() {
                        seen_handles.push(seen.to_string());
                    }
                }
                panic!(
                    "timed out waiting for feedback for {handle_id}; saw handles {seen_handles:?}"
                );
            }
        }
    }

    #[test]
    fn read_file_loads_embedded_stdlib_prompt_source() {
        let mut vm = vm();
        let source = call(
            &mut vm,
            "read_file",
            vec![s("std/agent/prompts/tool_contract_text.harn.prompt")],
        )
        .unwrap()
        .display();
        assert!(source.contains("{{ text_response_protocol }}"));
        assert!(source.contains("{{ native_contract }}"));
    }

    #[test]
    fn read_file_cache_invalidates_after_external_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.txt");
        std::fs::write(&path, "one").unwrap();
        let path_arg = path.to_string_lossy().into_owned();
        let mut vm = vm();

        assert_eq!(
            call(&mut vm, "read_file", vec![s(&path_arg)])
                .unwrap()
                .display(),
            "one"
        );
        std::fs::write(&path, "two updated").unwrap();

        assert_eq!(
            call(&mut vm, "read_file", vec![s(&path_arg)])
                .unwrap()
                .display(),
            "two updated"
        );
    }

    #[test]
    fn list_dir_observes_active_overlay_entries() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = std::sync::Arc::new(crate::testbench::overlay_fs::OverlayFs::rooted_at(
            dir.path(),
        ));
        let _guard = crate::testbench::overlay_fs::install_overlay(overlay);
        let mut vm = vm();
        let subdir = dir.path().join("overlay-dir");
        let file = subdir.join("created.txt");

        call(&mut vm, "mkdir", vec![s(&subdir.to_string_lossy())]).unwrap();
        call(
            &mut vm,
            "write_file",
            vec![s(&file.to_string_lossy()), s("overlay only")],
        )
        .unwrap();

        let listed = call(&mut vm, "list_dir", vec![s(&subdir.to_string_lossy())]).unwrap();
        let VmValue::List(items) = listed else {
            panic!("list_dir returns list");
        };
        let names = items.iter().map(VmValue::display).collect::<Vec<_>>();
        assert_eq!(names, vec!["created.txt".to_string()]);
        assert!(
            !file.exists(),
            "overlay write should not materialize on the underlying fs"
        );
    }

    #[test]
    fn read_lines_observes_active_overlay_content() {
        let dir = tempfile::tempdir().unwrap();
        let overlay = std::sync::Arc::new(crate::testbench::overlay_fs::OverlayFs::rooted_at(
            dir.path(),
        ));
        let _guard = crate::testbench::overlay_fs::install_overlay(overlay);
        let mut vm = vm();
        let path = dir.path().join("lines.txt");

        call(
            &mut vm,
            "write_file",
            vec![s(&path.to_string_lossy()), s("one\ntwo\n")],
        )
        .unwrap();

        let lines = call(&mut vm, "read_lines", vec![s(&path.to_string_lossy())]).unwrap();
        let VmValue::List(items) = lines else {
            panic!("read_lines returns list");
        };
        let lines = items.iter().map(VmValue::display).collect::<Vec<_>>();
        assert_eq!(lines, vec!["one".to_string(), "two".to_string()]);
        assert!(
            !path.exists(),
            "overlay write should not materialize on the underlying fs"
        );
    }

    #[test]
    fn walk_dir_long_running_returns_handle_and_feedback() {
        // Recover from a poisoned mutex: each test calls
        // `reset_state()` immediately below, so the previous test's
        // panic-shaped failure does not contaminate this one. Without
        // this, a single failing test cascades into PoisonError-driven
        // failures across every subsequent long-running test.
        let _guard = LONG_RUNNING_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::stdlib::long_running::reset_state();
        let _ = crate::orchestration::agent_inbox::drain("");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.harn"), "fn main() {}\n").unwrap();
        let mut vm = vm();

        let response = call(
            &mut vm,
            "walk_dir",
            vec![
                s(&dir.path().to_string_lossy()),
                dict(vec![("long_running", b(true))]),
            ],
        )
        .unwrap();
        let response = response.as_dict().expect("handle dict");
        assert_eq!(response["status"].display(), "running");
        assert_eq!(response["operation"].display(), "walk_dir");
        assert!(response["command_or_op_descriptor"]
            .display()
            .contains("walk_dir"));
        let handle_id = response["handle_id"].display();
        let payload = drain_feedback(&handle_id);

        assert_eq!(payload["status"], "completed");
        assert_eq!(payload["operation"], "walk_dir");
        assert!(payload["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"].as_str().unwrap().ends_with("src/lib.harn")));
    }

    #[test]
    fn glob_long_running_returns_handle_and_feedback() {
        // Recover from a poisoned mutex: each test calls
        // `reset_state()` immediately below, so the previous test's
        // panic-shaped failure does not contaminate this one. Without
        // this, a single failing test cascades into PoisonError-driven
        // failures across every subsequent long-running test.
        let _guard = LONG_RUNNING_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        crate::stdlib::long_running::reset_state();
        let _ = crate::orchestration::agent_inbox::drain("");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.harn"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# test\n").unwrap();
        let mut vm = vm();

        let response = call(
            &mut vm,
            "glob",
            vec![
                s("**/*.harn"),
                s(&dir.path().to_string_lossy()),
                dict(vec![("background", b(true))]),
            ],
        )
        .unwrap();
        let response = response.as_dict().expect("handle dict");
        assert_eq!(response["status"].display(), "running");
        assert_eq!(response["operation"].display(), "glob");
        let handle_id = response["handle_id"].display();
        let payload = drain_feedback(&handle_id);

        assert_eq!(payload["status"], "completed");
        let result = payload["result"].as_array().unwrap();
        assert_eq!(result.len(), 1);
        assert!(result[0].as_str().unwrap().ends_with("src/lib.harn"));
    }
}
