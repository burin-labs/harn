use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::{cell::RefCell, thread_local};

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

thread_local! {
    static FILE_TEXT_CACHE: RefCell<BTreeMap<PathBuf, Rc<str>>> = const { RefCell::new(BTreeMap::new()) };
}

pub(crate) fn reset_fs_state() {
    FILE_TEXT_CACHE.with(|cache| cache.borrow_mut().clear());
}

fn resolve_fs_path(path: &str) -> PathBuf {
    crate::stdlib::process::resolve_source_relative_path(path)
}

fn result_ok(value: VmValue) -> VmValue {
    VmValue::EnumVariant {
        enum_name: "Result".into(),
        variant: "Ok".into(),
        fields: vec![value],
    }
}

fn result_err(value: VmValue) -> VmValue {
    VmValue::EnumVariant {
        enum_name: "Result".into(),
        variant: "Err".into(),
        fields: vec![value],
    }
}

pub(crate) fn register_fs_builtins(vm: &mut Vm) {
    vm.register_builtin("read_file", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let resolved = resolve_fs_path(&path);
        if let Some(cached) = FILE_TEXT_CACHE.with(|cache| cache.borrow().get(&resolved).cloned()) {
            return Ok(VmValue::String(cached));
        }
        match std::fs::read_to_string(&resolved) {
            Ok(content) => {
                let shared: Rc<str> = Rc::from(content);
                FILE_TEXT_CACHE.with(|cache| {
                    cache.borrow_mut().insert(resolved.clone(), shared.clone());
                });
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
        if let Some(cached) = FILE_TEXT_CACHE.with(|cache| cache.borrow().get(&resolved).cloned()) {
            return Ok(result_ok(VmValue::String(cached)));
        }
        match std::fs::read_to_string(&resolved) {
            Ok(content) => {
                let shared: Rc<str> = Rc::from(content);
                FILE_TEXT_CACHE.with(|cache| {
                    cache.borrow_mut().insert(resolved.clone(), shared.clone());
                });
                Ok(result_ok(VmValue::String(shared)))
            }
            Err(e) => Ok(result_err(VmValue::String(Rc::from(format!(
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
            FILE_TEXT_CACHE.with(|cache| {
                cache.borrow_mut().insert(resolved, Rc::from(content));
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
