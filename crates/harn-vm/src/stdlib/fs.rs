use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_fs_builtins(vm: &mut Vm) {
    vm.register_builtin("read_file", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        match std::fs::read_to_string(&path) {
            Ok(content) => Ok(VmValue::String(Rc::from(content))),
            Err(e) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to read file {path}: {e}"
            ))))),
        }
    });

    vm.register_builtin("write_file", |args, _out| {
        if args.len() >= 2 {
            let path = args[0].display();
            let content = args[1].display();
            std::fs::write(&path, &content).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to write file {path}: {e}"
                ))))
            })?;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("file_exists", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::Bool(std::path::Path::new(&path).exists()))
    });

    vm.register_builtin("delete_file", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let p = std::path::Path::new(&path);
        if p.is_dir() {
            std::fs::remove_dir_all(&path).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to delete directory {path}: {e}"
                ))))
            })?;
        } else {
            std::fs::remove_file(&path).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to delete file {path}: {e}"
                ))))
            })?;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("append_file", |args, _out| {
        use std::io::Write;
        if args.len() >= 2 {
            let path = args[0].display();
            let content = args[1].display();
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)
                .map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "Failed to open file {path}: {e}"
                    ))))
                })?;
            file.write_all(content.as_bytes()).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to append to file {path}: {e}"
                ))))
            })?;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("list_dir", |args, _out| {
        let path = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| ".".to_string());
        let entries = std::fs::read_dir(&path).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to list directory {path}: {e}"
            ))))
        })?;
        let mut result = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|e| VmError::Thrown(VmValue::String(Rc::from(e.to_string()))))?;
            let name = entry.file_name().to_string_lossy().to_string();
            result.push(VmValue::String(Rc::from(name)));
        }
        result.sort_by_key(|a| a.display());
        Ok(VmValue::List(Rc::new(result)))
    });

    vm.register_builtin("mkdir", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        std::fs::create_dir_all(&path).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to create directory {path}: {e}"
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
            path.to_string_lossy().to_string().as_str(),
        )))
    });

    vm.register_builtin("copy_file", |args, _out| {
        if args.len() >= 2 {
            let src = args[0].display();
            let dst = args[1].display();
            std::fs::copy(&src, &dst).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to copy {src} to {dst}: {e}"
                ))))
            })?;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("temp_dir", |_args, _out| {
        Ok(VmValue::String(Rc::from(
            std::env::temp_dir().to_string_lossy().to_string().as_str(),
        )))
    });

    vm.register_builtin("stat", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let metadata = std::fs::metadata(&path).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to stat {path}: {e}"
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
