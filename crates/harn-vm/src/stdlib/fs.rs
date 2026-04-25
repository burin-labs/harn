use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
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

fn resolve_fs_path(path: &str) -> PathBuf {
    crate::stdlib::process::resolve_source_relative_path(path)
}

fn result_ok(value: VmValue) -> VmValue {
    VmValue::enum_variant("Result", "Ok", vec![value])
}

fn result_err(value: VmValue) -> VmValue {
    VmValue::enum_variant("Result", "Err", vec![value])
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
        let mut max_depth: Option<usize> = None;
        let mut follow_symlinks = false;
        if let Some(VmValue::Dict(opts)) = args.get(1) {
            if let Some(v) = opts.get("max_depth").and_then(|v| v.as_int()) {
                if v >= 0 {
                    max_depth = Some(v as usize);
                }
            }
            if let Some(VmValue::Bool(b)) = opts.get("follow_symlinks") {
                follow_symlinks = *b;
            }
        }
        let mut walker = walkdir::WalkDir::new(&resolved).follow_links(follow_symlinks);
        if let Some(d) = max_depth {
            walker = walker.max_depth(d);
        }
        let mut entries: Vec<VmValue> = Vec::new();
        for entry in walker.into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            let depth = entry.depth() as i64;
            let is_dir = entry.file_type().is_dir();
            let mut dict = BTreeMap::new();
            dict.insert(
                "path".to_string(),
                VmValue::String(Rc::from(path.to_string_lossy().replace('\\', "/"))),
            );
            dict.insert("is_dir".to_string(), VmValue::Bool(is_dir));
            dict.insert(
                "is_file".to_string(),
                VmValue::Bool(entry.file_type().is_file()),
            );
            dict.insert("depth".to_string(), VmValue::Int(depth));
            entries.push(VmValue::Dict(Rc::new(dict)));
        }
        Ok(VmValue::List(Rc::new(entries)))
    });

    vm.register_builtin("glob", |args, _out| {
        let pattern = args.first().map(|a| a.display()).unwrap_or_default();
        if pattern.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "glob: pattern is required",
            ))));
        }
        // The pattern is matched against paths relative to the configured
        // base. Default base is the script source directory; an explicit
        // second argument overrides.
        let base_str = args
            .get(1)
            .map(|a| a.display())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| ".".to_string());
        let base = resolve_fs_path(&base_str);
        crate::stdlib::sandbox::enforce_fs_path(
            "glob",
            &base,
            crate::stdlib::sandbox::FsAccess::Read,
        )?;
        let mut builder = globset::GlobSetBuilder::new();
        let glob = globset::Glob::new(&pattern)
            .map_err(|e| VmError::Thrown(VmValue::String(Rc::from(format!("glob: {e}")))))?;
        builder.add(glob);
        let set = builder
            .build()
            .map_err(|e| VmError::Thrown(VmValue::String(Rc::from(format!("glob: {e}")))))?;
        let mut matches: Vec<VmValue> = Vec::new();
        for entry in walkdir::WalkDir::new(&base)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            // Match against path relative to base, using forward slashes.
            let rel = match entry.path().strip_prefix(&base) {
                Ok(p) => p.to_path_buf(),
                Err(_) => continue,
            };
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if rel_str.is_empty() {
                continue;
            }
            if set.is_match(&rel_str) {
                matches.push(VmValue::String(Rc::from(
                    entry.path().to_string_lossy().replace('\\', "/"),
                )));
            }
        }
        matches.sort_by_key(|a| a.display());
        Ok(VmValue::List(Rc::new(matches)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
