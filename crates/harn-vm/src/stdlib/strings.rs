use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{values_equal, VmError, VmValue};
use crate::vm::Vm;

fn template_truthy(value: &VmValue) -> bool {
    match value {
        VmValue::Nil => false,
        VmValue::Bool(v) => *v,
        VmValue::Int(v) => *v != 0,
        VmValue::Float(v) => *v != 0.0,
        VmValue::String(v) => !v.trim().is_empty(),
        VmValue::List(items) => !items.is_empty(),
        VmValue::Dict(items) => !items.is_empty(),
        _ => true,
    }
}

fn render_template_segment(
    template: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
    start: usize,
    stop_on_end: bool,
) -> (String, usize) {
    let mut rendered = String::with_capacity(template.len().saturating_sub(start));
    let mut cursor = start;
    while let Some(open_rel) = template[cursor..].find("{{") {
        let open = cursor + open_rel;
        rendered.push_str(&template[cursor..open]);
        let Some(close_rel) = template[open + 2..].find("}}") else {
            rendered.push_str(&template[open..]);
            return (rendered, template.len());
        };
        let close = open + 2 + close_rel;
        let token = template[open + 2..close].trim();
        cursor = close + 2;

        if token == "end" {
            if stop_on_end {
                return (rendered, cursor);
            }
            rendered.push_str(&template[open..cursor]);
            continue;
        }

        if let Some(key) = token.strip_prefix("if ").map(str::trim) {
            let (inner, next_cursor) = render_template_segment(template, bindings, cursor, true);
            if bindings
                .and_then(|map| map.get(key))
                .is_some_and(template_truthy)
            {
                rendered.push_str(&inner);
            }
            cursor = next_cursor;
            continue;
        }

        if let Some(value) = bindings.and_then(|map| map.get(token)) {
            rendered.push_str(&value.display());
        } else {
            rendered.push_str(&template[open..cursor]);
        }
    }
    rendered.push_str(&template[cursor..]);
    (rendered, template.len())
}

pub(crate) fn render_template_text(
    template: &str,
    bindings: Option<&BTreeMap<String, VmValue>>,
) -> String {
    render_template_segment(template, bindings, 0, false).0
}

pub(crate) fn register_string_builtins(vm: &mut Vm) {
    vm.register_builtin("format", |args, _out| {
        let template = args.first().map(|a| a.display()).unwrap_or_default();

        // If the second argument is a dict, use named placeholders {key}
        if let Some(dict) = args.get(1).and_then(|a| a.as_dict()) {
            // Build result by scanning for {name} patterns and replacing them
            // in a single pass to avoid double-substitution.
            let mut result = String::with_capacity(template.len());
            let mut rest = template.as_str();
            while let Some(open) = rest.find('{') {
                result.push_str(&rest[..open]);
                if let Some(close) = rest[open..].find('}') {
                    let key = &rest[open + 1..open + close];
                    if let Some(val) = dict.get(key) {
                        result.push_str(&val.display());
                    } else {
                        // Keep unmatched placeholders as-is
                        result.push_str(&rest[open..open + close + 1]);
                    }
                    rest = &rest[open + close + 1..];
                } else {
                    result.push_str(&rest[open..]);
                    rest = "";
                    break;
                }
            }
            result.push_str(rest);
            return Ok(VmValue::String(Rc::from(result)));
        }

        // Otherwise, use positional {} placeholders
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
        Ok(VmValue::String(Rc::from(result)))
    });

    vm.register_builtin("trim", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(s.trim())))
    });

    vm.register_builtin("lowercase", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(s.to_lowercase())))
    });

    vm.register_builtin("uppercase", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(s.to_uppercase())))
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
        Ok(VmValue::String(Rc::from(s.replace(&old, &new))))
    });

    vm.register_builtin("join", |args, _out| {
        let sep = args.get(1).map(|a| a.display()).unwrap_or_default();
        match args.first() {
            Some(VmValue::List(items)) => {
                let parts: Vec<String> = items.iter().map(|v| v.display()).collect();
                Ok(VmValue::String(Rc::from(parts.join(&sep))))
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
            Some(ext) => Ok(VmValue::String(Rc::from(format!(
                ".{}",
                ext.to_string_lossy()
            )))),
            None => Ok(VmValue::String(Rc::from(""))),
        }
    });

    // --- Template rendering ---

    vm.register_builtin("render", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let resolved = crate::stdlib::process::resolve_source_relative_path(&path);
        let template = std::fs::read_to_string(&resolved).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to read template {}: {e}",
                resolved.display()
            ))))
        })?;
        if let Some(bindings) = args.get(1).and_then(|a| a.as_dict()) {
            Ok(VmValue::String(Rc::from(render_template_text(
                &template,
                Some(bindings),
            ))))
        } else {
            Ok(VmValue::String(Rc::from(render_template_text(
                &template, None,
            ))))
        }
    });
}
