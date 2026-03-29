use std::rc::Rc;

use crate::value::{values_equal, VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_string_builtins(vm: &mut Vm) {
    vm.register_builtin("format", |args, _out| {
        let template = args.first().map(|a| a.display()).unwrap_or_default();
        let mut result = String::with_capacity(template.len());
        let mut arg_iter = args.iter().skip(1);
        let mut rest = template.as_str();
        while let Some(pos) = rest.find("{}") {
            result.push_str(&rest[..pos]);
            if let Some(arg) = arg_iter.next() {
                result.push_str(&arg.display());
            } else {
                result.push_str("{}");
            }
            rest = &rest[pos + 2..];
        }
        result.push_str(rest);
        Ok(VmValue::String(Rc::from(result.as_str())))
    });

    vm.register_builtin("trim", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(s.trim())))
    });

    vm.register_builtin("lowercase", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(s.to_lowercase().as_str())))
    });

    vm.register_builtin("uppercase", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(s.to_uppercase().as_str())))
    });

    vm.register_builtin("split", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let sep = args
            .get(1)
            .map(|a| a.display())
            .unwrap_or_else(|| " ".to_string());
        let parts: Vec<VmValue> = s
            .split(&sep)
            .map(|p| VmValue::String(Rc::from(p)))
            .collect();
        Ok(VmValue::List(Rc::new(parts)))
    });

    vm.register_builtin("starts_with", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let prefix = args.get(1).map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::Bool(s.starts_with(&prefix)))
    });

    vm.register_builtin("ends_with", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let suffix = args.get(1).map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::Bool(s.ends_with(&suffix)))
    });

    vm.register_builtin("contains", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::String(s) => {
                let substr = args.get(1).map(|a| a.display()).unwrap_or_default();
                Ok(VmValue::Bool(s.contains(&substr)))
            }
            VmValue::List(items) => {
                let target = args.get(1).unwrap_or(&VmValue::Nil);
                Ok(VmValue::Bool(
                    items.iter().any(|item| values_equal(item, target)),
                ))
            }
            _ => Ok(VmValue::Bool(false)),
        }
    });

    vm.register_builtin("replace", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let old = args.get(1).map(|a| a.display()).unwrap_or_default();
        let new = args.get(2).map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(s.replace(&old, &new).as_str())))
    });

    vm.register_builtin("join", |args, _out| {
        let sep = args.get(1).map(|a| a.display()).unwrap_or_default();
        match args.first() {
            Some(VmValue::List(items)) => {
                let parts: Vec<String> = items.iter().map(|v| v.display()).collect();
                Ok(VmValue::String(Rc::from(parts.join(&sep).as_str())))
            }
            _ => Ok(VmValue::String(Rc::from(""))),
        }
    });

    vm.register_builtin("substring", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let start = args.get(1).and_then(|a| a.as_int()).unwrap_or(0).max(0) as usize;
        let chars: Vec<char> = s.chars().collect();
        let start = start.min(chars.len());
        match args.get(2).and_then(|a| a.as_int()) {
            Some(length) => {
                let length = (length.max(0) as usize).min(chars.len() - start);
                let result: String = chars[start..start + length].iter().collect();
                Ok(VmValue::String(Rc::from(result)))
            }
            None => {
                let result: String = chars[start..].iter().collect();
                Ok(VmValue::String(Rc::from(result)))
            }
        }
    });

    // --- Path builtins ---

    vm.register_builtin("dirname", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let p = std::path::Path::new(&path);
        match p.parent() {
            Some(parent) => Ok(VmValue::String(Rc::from(parent.to_string_lossy().as_ref()))),
            None => Ok(VmValue::String(Rc::from(""))),
        }
    });

    vm.register_builtin("basename", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let p = std::path::Path::new(&path);
        match p.file_name() {
            Some(name) => Ok(VmValue::String(Rc::from(name.to_string_lossy().as_ref()))),
            None => Ok(VmValue::String(Rc::from(""))),
        }
    });

    vm.register_builtin("extname", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let p = std::path::Path::new(&path);
        match p.extension() {
            Some(ext) => Ok(VmValue::String(Rc::from(
                format!(".{}", ext.to_string_lossy()).as_str(),
            ))),
            None => Ok(VmValue::String(Rc::from(""))),
        }
    });

    // --- Template rendering ---

    vm.register_builtin("render", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let template = std::fs::read_to_string(&path).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to read template {path}: {e}"
            ))))
        })?;
        if let Some(bindings) = args.get(1).and_then(|a| a.as_dict()) {
            let mut result = template;
            for (key, val) in bindings.iter() {
                result = result.replace(&format!("{{{{{key}}}}}"), &val.display());
            }
            Ok(VmValue::String(Rc::from(result)))
        } else {
            Ok(VmValue::String(Rc::from(template)))
        }
    });
}
