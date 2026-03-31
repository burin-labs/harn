use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_json_builtins(vm: &mut Vm) {
    vm.register_builtin("json_stringify", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::String(Rc::from(vm_value_to_json(val))))
    });

    vm.register_builtin("json_parse", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(jv) => Ok(json_to_vm_value(&jv)),
            Err(e) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "JSON parse error: {e}"
            ))))),
        }
    });

    vm.register_builtin("json_validate", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "json_validate requires 2 arguments: data and schema",
            ))));
        }
        let data = &args[0];
        let schema = &args[1];
        let schema_dict = match schema.as_dict() {
            Some(d) => d,
            None => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "json_validate: schema must be a dict",
                ))));
            }
        };
        let mut errors = Vec::new();
        validate_value(data, schema_dict, "", &mut errors);
        if errors.is_empty() {
            Ok(VmValue::Bool(true))
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                errors.join("; "),
            ))))
        }
    });

    vm.register_builtin("json_extract", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "json_extract requires at least 1 argument: text",
            ))));
        }
        let text = args[0].display();
        let key = args.get(1).map(|a| a.display());

        let json_str = extract_json_from_text(&text);
        let parsed = match serde_json::from_str::<serde_json::Value>(&json_str) {
            Ok(jv) => json_to_vm_value(&jv),
            Err(e) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "json_extract: failed to parse JSON: {e}"
                )))));
            }
        };

        match key {
            Some(k) => match &parsed {
                VmValue::Dict(map) => match map.get(&k) {
                    Some(val) => Ok(val.clone()),
                    None => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "json_extract: key '{}' not found",
                        k
                    ))))),
                },
                _ => Err(VmError::Thrown(VmValue::String(Rc::from(
                    "json_extract: parsed value is not a dict, cannot extract key",
                )))),
            },
            None => Ok(parsed),
        }
    });
}

// =============================================================================
// JSON conversion helpers (pub(crate) for use by other modules)
// =============================================================================

pub(crate) fn escape_json_string_vm(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub(crate) fn vm_value_to_json(val: &VmValue) -> String {
    match val {
        VmValue::String(s) => escape_json_string_vm(s),
        VmValue::Int(n) => n.to_string(),
        VmValue::Float(n) => n.to_string(),
        VmValue::Bool(b) => b.to_string(),
        VmValue::Nil => "null".to_string(),
        VmValue::List(items) => {
            let inner: Vec<String> = items.iter().map(vm_value_to_json).collect();
            format!("[{}]", inner.join(","))
        }
        VmValue::Dict(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}:{}", escape_json_string_vm(k), vm_value_to_json(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        VmValue::Set(items) => {
            let inner: Vec<String> = items.iter().map(vm_value_to_json).collect();
            format!("[{}]", inner.join(","))
        }
        _ => "null".to_string(),
    }
}

pub(crate) fn json_to_vm_value(jv: &serde_json::Value) -> VmValue {
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
        serde_json::Value::String(s) => VmValue::String(Rc::from(s.as_str())),
        serde_json::Value::Array(arr) => {
            VmValue::List(Rc::new(arr.iter().map(json_to_vm_value).collect()))
        }
        serde_json::Value::Object(map) => {
            let mut m = BTreeMap::new();
            for (k, v) in map {
                m.insert(k.clone(), json_to_vm_value(v));
            }
            VmValue::Dict(Rc::new(m))
        }
    }
}

fn validate_value(
    value: &VmValue,
    schema: &BTreeMap<String, VmValue>,
    path: &str,
    errors: &mut Vec<String>,
) {
    if let Some(VmValue::String(expected_type)) = schema.get("type") {
        let actual_type = value.type_name();
        let type_str: &str = expected_type;
        if type_str != "any" && actual_type != type_str {
            let location = if path.is_empty() {
                "root".to_string()
            } else {
                path.to_string()
            };
            errors.push(format!(
                "at {}: expected type '{}', got '{}'",
                location, type_str, actual_type
            ));
            return;
        }
    }

    if let Some(VmValue::List(required_keys)) = schema.get("required") {
        if let VmValue::Dict(map) = value {
            for key_val in required_keys.iter() {
                let key = key_val.display();
                if !map.contains_key(&key) {
                    let location = if path.is_empty() {
                        "root".to_string()
                    } else {
                        path.to_string()
                    };
                    errors.push(format!("at {}: missing required key '{}'", location, key));
                }
            }
        }
    }

    if let Some(VmValue::Dict(prop_schemas)) = schema.get("properties") {
        if let VmValue::Dict(map) = value {
            for (key, prop_schema) in prop_schemas.iter() {
                if let Some(prop_value) = map.get(key) {
                    if let Some(prop_schema_dict) = prop_schema.as_dict() {
                        let child_path = if path.is_empty() {
                            key.clone()
                        } else {
                            format!("{}.{}", path, key)
                        };
                        validate_value(prop_value, prop_schema_dict, &child_path, errors);
                    }
                }
            }
        }
    }

    if let Some(VmValue::Dict(item_schema)) = schema.get("items") {
        if let VmValue::List(items) = value {
            for (i, item) in items.iter().enumerate() {
                let child_path = if path.is_empty() {
                    format!("[{}]", i)
                } else {
                    format!("{}[{}]", path, i)
                };
                validate_value(item, item_schema, &child_path, errors);
            }
        }
    }
}

pub(crate) fn extract_json_from_text(text: &str) -> String {
    let trimmed = text.trim();

    // 1. Try code-fence extraction first (```json ... ```)
    if let Some(start) = trimmed.find("```") {
        let after_backticks = &trimmed[start + 3..];
        let content_start = if let Some(nl) = after_backticks.find('\n') {
            nl + 1
        } else {
            0
        };
        let content = &after_backticks[content_start..];
        if let Some(end) = content.find("```") {
            return content[..end].trim().to_string();
        }
    }

    // 2. Try to find a balanced JSON object or array
    if let Some(result) = find_balanced_json(trimmed, b'{', b'}') {
        return result;
    }
    if let Some(result) = find_balanced_json(trimmed, b'[', b']') {
        return result;
    }

    trimmed.to_string()
}

/// Find the first balanced JSON structure delimited by `open`/`close` chars.
/// Respects string literals (skipping brackets inside "...") and nesting.
fn find_balanced_json(text: &str, open: u8, close: u8) -> Option<String> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(|&b| b == open)?;

    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    let mut i = start;

    while i < bytes.len() {
        let b = bytes[i];
        if escape {
            // Handle \uXXXX: skip the 4 hex digits after \u
            if b == b'u' && i + 4 < bytes.len() {
                i += 5; // skip 'u' + 4 hex digits
            } else {
                i += 1;
            }
            escape = false;
            continue;
        }
        if b == b'\\' && in_string {
            escape = true;
            i += 1;
            continue;
        }
        if b == b'"' {
            in_string = !in_string;
        } else if !in_string {
            if b == open {
                depth += 1;
            } else if b == close {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..=i].to_string());
                }
            }
        }
        i += 1;
    }
    None
}
