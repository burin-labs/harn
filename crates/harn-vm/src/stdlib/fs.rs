use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use std::{cell::RefCell, thread_local};

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

thread_local! {
    static FILE_TEXT_CACHE: RefCell<BTreeMap<PathBuf, FileTextCacheEntry>> = const { RefCell::new(BTreeMap::new()) };
}

const FILE_TEXT_CACHE_MAX_ENTRIES: usize = 256;

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
    vm.register_builtin("read_file", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let resolved = resolve_fs_path(&path);
        crate::stdlib::sandbox::enforce_fs_path(
            "read_file",
            &resolved,
            crate::stdlib::sandbox::FsAccess::Read,
        )?;
        if let Some(cached) = read_cached_text(&resolved) {
            return Ok(VmValue::String(cached));
        }
        match std::fs::read_to_string(&resolved) {
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
    });

    vm.register_builtin("read_file_result", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
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
        match std::fs::read_to_string(&resolved) {
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
    });

    vm.register_builtin("read_file_bytes", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let resolved = resolve_fs_path(&path);
        crate::stdlib::sandbox::enforce_fs_path(
            "read_file_bytes",
            &resolved,
            crate::stdlib::sandbox::FsAccess::Read,
        )?;
        match std::fs::read(&resolved) {
            Ok(content) => Ok(VmValue::Bytes(Rc::new(content))),
            Err(e) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to read file {}: {e}",
                resolved.display()
            ))))),
        }
    });

    vm.register_builtin("write_file", |args, _out| {
        if args.len() >= 2 {
            let path = args[0].display();
            let content = args[1].display();
            let resolved = resolve_fs_path(&path);
            crate::stdlib::sandbox::enforce_fs_path(
                "write_file",
                &resolved,
                crate::stdlib::sandbox::FsAccess::Write,
            )?;
            std::fs::write(&resolved, &content).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to write file {}: {e}",
                    resolved.display()
                ))))
            })?;
            write_cached_text(resolved, Rc::from(content));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("write_file_bytes", |args, _out| {
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
            crate::stdlib::sandbox::enforce_fs_path(
                "write_file_bytes",
                &resolved,
                crate::stdlib::sandbox::FsAccess::Write,
            )?;
            std::fs::write(&resolved, content).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to write file {}: {e}",
                    resolved.display()
                ))))
            })?;
            FILE_TEXT_CACHE.with(|cache| {
                cache.borrow_mut().remove(&resolved);
            });
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("file_exists", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let resolved = resolve_fs_path(&path);
        crate::stdlib::sandbox::enforce_fs_path(
            "file_exists",
            &resolved,
            crate::stdlib::sandbox::FsAccess::Read,
        )?;
        Ok(VmValue::Bool(resolved.exists()))
    });

    vm.register_builtin("delete_file", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let resolved = resolve_fs_path(&path);
        crate::stdlib::sandbox::enforce_fs_path(
            "delete_file",
            &resolved,
            crate::stdlib::sandbox::FsAccess::Delete,
        )?;
        if resolved.is_dir() {
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
    });

    vm.register_builtin("append_file", |args, _out| {
        use std::io::Write;
        if args.len() >= 2 {
            let path = args[0].display();
            let content = args[1].display();
            let resolved = resolve_fs_path(&path);
            crate::stdlib::sandbox::enforce_fs_path(
                "append_file",
                &resolved,
                crate::stdlib::sandbox::FsAccess::Write,
            )?;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&resolved)
                .map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "Failed to open file {}: {e}",
                        resolved.display()
                    ))))
                })?;
            file.write_all(content.as_bytes()).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to append to file {}: {e}",
                    resolved.display()
                ))))
            })?;
            FILE_TEXT_CACHE.with(|cache| {
                cache.borrow_mut().remove(&resolved);
            });
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("list_dir", |args, _out| {
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
        let entries = std::fs::read_dir(&resolved).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to list directory {}: {e}",
                resolved.display()
            ))))
        })?;
        let mut result = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|e| VmError::Thrown(VmValue::String(Rc::from(e.to_string()))))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            result.push(VmValue::String(Rc::from(name)));
        }
        result.sort_by_key(|a| a.display());
        Ok(VmValue::List(Rc::new(result)))
    });

    vm.register_builtin("mkdir", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let resolved = resolve_fs_path(&path);
        crate::stdlib::sandbox::enforce_fs_path(
            "mkdir",
            &resolved,
            crate::stdlib::sandbox::FsAccess::Write,
        )?;
        std::fs::create_dir_all(&resolved).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to create directory {}: {e}",
                resolved.display()
            ))))
        })?;
        Ok(VmValue::Nil)
    });

    vm.register_builtin("path_join", |args, _out| {
        let mut path = std::path::PathBuf::new();
        for arg in args {
            path.push(arg.display());
        }
        Ok(VmValue::String(Rc::from(
            path.to_string_lossy().into_owned().as_str(),
        )))
    });

    vm.register_builtin("copy_file", |args, _out| {
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
    });

    vm.register_builtin("temp_dir", |_args, _out| {
        Ok(VmValue::String(Rc::from(
            std::env::temp_dir().to_string_lossy().into_owned().as_str(),
        )))
    });

    vm.register_builtin("stat", |args, _out| {
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
    });

    // --- scripting-polish additions ----------------------------------

    vm.register_builtin("move_file", |args, _out| {
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
        // Try rename first (fast, atomic on the same filesystem). Fall
        // back to copy+delete which crosses filesystems.
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
    });

    vm.register_builtin("read_lines", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let resolved = resolve_fs_path(&path);
        crate::stdlib::sandbox::enforce_fs_path(
            "read_lines",
            &resolved,
            crate::stdlib::sandbox::FsAccess::Read,
        )?;
        let content = std::fs::read_to_string(&resolved).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "read_lines: {}: {e}",
                resolved.display()
            ))))
        })?;
        // split_terminator drops the trailing empty element from a final
        // newline; lines() handles \r\n correctly.
        let lines: Vec<VmValue> = content
            .lines()
            .map(|l| VmValue::String(Rc::from(l)))
            .collect();
        Ok(VmValue::List(Rc::new(lines)))
    });

    vm.register_builtin("walk_dir", |args, _out| {
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
    });

    vm.register_builtin("glob", |args, _out| {
        let pattern = args.first().map(|a| a.display()).unwrap_or_default();
        if pattern.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "glob: pattern is required",
            ))));
        }
        let options = parse_glob_options(args);
        let base = resolve_fs_path(&options.base);
        crate::stdlib::sandbox::enforce_fs_path(
            "glob",
            &base,
            crate::stdlib::sandbox::FsAccess::Read,
        )?;
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
    });
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
        for _ in 0..50 {
            for (kind, content) in crate::llm::drain_global_pending_feedback("") {
                assert_eq!(kind, "tool_result");
                let payload: serde_json::Value = serde_json::from_str(&content).unwrap();
                if payload["handle_id"] == handle_id {
                    return payload;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("timed out waiting for feedback for {handle_id}");
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
    fn walk_dir_long_running_returns_handle_and_feedback() {
        let _guard = LONG_RUNNING_TEST_LOCK.lock().unwrap();
        crate::stdlib::long_running::reset_state();
        let _ = crate::llm::drain_global_pending_feedback("");
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
        let _guard = LONG_RUNNING_TEST_LOCK.lock().unwrap();
        crate::stdlib::long_running::reset_state();
        let _ = crate::llm::drain_global_pending_feedback("");
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
