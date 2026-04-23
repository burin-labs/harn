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
        Ok(VmValue::Bool(resolved.exists()))
    });

    vm.register_builtin("delete_file", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let resolved = resolve_fs_path(&path);
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
