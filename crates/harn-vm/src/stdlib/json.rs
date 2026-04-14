use std::rc::Rc;
use std::{cell::RefCell, collections::BTreeMap, thread_local};

use crate::schema;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

thread_local! {
    static JSON_PARSE_CACHE: RefCell<BTreeMap<String, VmValue>> = const { RefCell::new(BTreeMap::new()) };
}

pub(crate) fn reset_json_state() {
    JSON_PARSE_CACHE.with(|cache| cache.borrow_mut().clear());
}

fn require_args(args: &[VmValue], min: usize, name: &str) -> Result<(), VmError> {
    if args.len() < min {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{name} requires {min} arguments"
        )))));
    }
    Ok(())
}

fn schema_key_list(value: &VmValue, builtin_name: &str) -> Result<Vec<String>, VmError> {
    let list = match value {
        VmValue::List(list) => list,
        _ => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "{builtin_name}: keys must be a list"
            )))));
        }
    };
    Ok(list.iter().map(VmValue::display).collect())
}

pub(crate) fn register_json_builtins(vm: &mut Vm) {
    vm.register_builtin("json_stringify", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::String(Rc::from(vm_value_to_json(val))))
    });

    vm.register_builtin("json_parse", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        if let Some(cached) = JSON_PARSE_CACHE.with(|cache| cache.borrow().get(&text).cloned()) {
            return Ok(cached);
        }
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(jv) => {
                let parsed = schema::json_to_vm_value(&jv);
                JSON_PARSE_CACHE.with(|cache| {
                    cache.borrow_mut().insert(text, parsed.clone());
                });
                Ok(parsed)
            }
            Err(e) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "JSON parse error: {e}"
            ))))),
        }
    });

    vm.register_builtin("yaml_parse", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        match serde_yaml::from_str::<serde_yaml::Value>(&text) {
            Ok(value) => match serde_json::to_value(value) {
                Ok(json_value) => Ok(schema::json_to_vm_value(&json_value)),
                Err(error) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "yaml_parse: {error}"
                ))))),
            },
            Err(error) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "YAML parse error: {error}"
            ))))),
        }
    });

    vm.register_builtin("yaml_stringify", |args, _out| {
        let value = args.first().unwrap_or(&VmValue::Nil);
        let data_value = vm_value_to_data_value(value);
        serde_yaml::to_string(&data_value)
            .map(|text| VmValue::String(Rc::from(text)))
            .map_err(|error| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "yaml_stringify: {error}"
                ))))
            })
    });

    vm.register_builtin("toml_parse", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        match text.parse::<toml::Value>() {
            Ok(value) => match serde_json::to_value(value) {
                Ok(json_value) => Ok(schema::json_to_vm_value(&json_value)),
                Err(error) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "toml_parse: {error}"
                ))))),
            },
            Err(error) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "TOML parse error: {error}"
            ))))),
        }
    });

    vm.register_builtin("toml_stringify", |args, _out| {
        let value = args.first().unwrap_or(&VmValue::Nil);
        let data_value = vm_value_to_data_value(value);
        let toml_value = toml::Value::try_from(data_value).map_err(|error| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "toml_stringify: {error}"
            ))))
        })?;
        toml::to_string(&toml_value)
            .map(|text| VmValue::String(Rc::from(text)))
            .map_err(|error| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "toml_stringify: {error}"
                ))))
            })
    });

    vm.register_builtin("json_validate", |args, _out| {
        require_args(args, 2, "json_validate")?;
        let result = schema::schema_expect_value(&args[0], &args[1], false);
        match result {
            Ok(_) => Ok(VmValue::Bool(true)),
            Err(error) => Err(error),
        }
    });

    vm.register_builtin("schema_check", |args, _out| {
        require_args(args, 2, "schema_check")?;
        Ok(schema::schema_result_value(&args[0], &args[1], false))
    });

    vm.register_builtin("schema_parse", |args, _out| {
        require_args(args, 2, "schema_parse")?;
        Ok(schema::schema_result_value(&args[0], &args[1], true))
    });

    vm.register_builtin("schema_is", |args, _out| {
        require_args(args, 2, "schema_is")?;
        Ok(VmValue::Bool(schema::schema_is_value(&args[0], &args[1])?))
    });

    vm.register_builtin("is_type", |args, _out| {
        require_args(args, 2, "is_type")?;
        Ok(VmValue::Bool(schema::schema_is_value(&args[0], &args[1])?))
    });

    vm.register_builtin("schema_expect", |args, _out| {
        require_args(args, 2, "schema_expect")?;
        let apply_defaults = args.get(2).is_some_and(|value| value.is_truthy());
        schema::schema_expect_value(&args[0], &args[1], apply_defaults)
    });

    vm.register_builtin("schema_to_json_schema", |args, _out| {
        require_args(args, 1, "schema_to_json_schema")?;
        schema::schema_to_json_schema_value(&args[0])
    });

    vm.register_builtin("schema_from_json_schema", |args, _out| {
        require_args(args, 1, "schema_from_json_schema")?;
        schema::schema_from_json_schema_value(&args[0])
    });

    vm.register_builtin("schema_to_openapi_schema", |args, _out| {
        require_args(args, 1, "schema_to_openapi_schema")?;
        schema::schema_to_openapi_schema_value(&args[0])
    });

    vm.register_builtin("schema_from_openapi_schema", |args, _out| {
        require_args(args, 1, "schema_from_openapi_schema")?;
        schema::schema_from_openapi_schema_value(&args[0])
    });

    vm.register_builtin("schema_extend", |args, _out| {
        require_args(args, 2, "schema_extend")?;
        schema::schema_extend_value(&args[0], &args[1])
    });

    vm.register_builtin("schema_partial", |args, _out| {
        require_args(args, 1, "schema_partial")?;
        schema::schema_partial_value(&args[0])
    });

    vm.register_builtin("schema_pick", |args, _out| {
        require_args(args, 2, "schema_pick")?;
        let keys = schema_key_list(&args[1], "schema_pick")?;
        schema::schema_pick_value(&args[0], &keys)
    });

    vm.register_builtin("schema_omit", |args, _out| {
        require_args(args, 2, "schema_omit")?;
        let keys = schema_key_list(&args[1], "schema_omit")?;
        schema::schema_omit_value(&args[0], &keys)
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
            Ok(jv) => schema::json_to_vm_value(&jv),
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

fn vm_value_to_data_value(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Int(i) => serde_json::json!(i),
        VmValue::Float(f) => serde_json::json!(f),
        VmValue::String(s) => serde_json::json!(s.as_ref()),
        VmValue::Bool(b) => serde_json::json!(b),
        VmValue::Nil => serde_json::Value::Null,
        VmValue::List(items) | VmValue::Set(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_data_value).collect())
        }
        VmValue::Dict(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), vm_value_to_data_value(value)))
                .collect(),
        ),
        VmValue::StructInstance { fields, .. } => serde_json::Value::Object(
            fields
                .iter()
                .map(|(key, value)| (key.clone(), vm_value_to_data_value(value)))
                .collect(),
        ),
        // Ranges stringify like Display (`"1 to 5"`); use `.to_list()` in Harn
        // to materialise an int array.
        VmValue::Range(_) => serde_json::json!(value.display()),
        _ => serde_json::json!(value.display()),
    }
}

pub(crate) fn vm_value_to_json(val: &VmValue) -> String {
    let mut out = String::new();
    write_vm_value_to_json(val, &mut out);
    out
}

fn write_vm_value_to_json(val: &VmValue, out: &mut String) {
    match val {
        VmValue::String(s) => out.push_str(&escape_json_string_vm(s)),
        VmValue::Int(n) => out.push_str(&n.to_string()),
        VmValue::Float(n) => out.push_str(&n.to_string()),
        VmValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        VmValue::Nil => out.push_str("null"),
        VmValue::List(items) | VmValue::Set(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_vm_value_to_json(item, out);
            }
            out.push(']');
        }
        VmValue::Dict(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&escape_json_string_vm(k));
                out.push(':');
                write_vm_value_to_json(v, out);
            }
            out.push('}');
        }
        VmValue::Range(_) => out.push_str(&escape_json_string_vm(&val.display())),
        _ => out.push_str("null"),
    }
}

pub(crate) fn extract_json_from_text(text: &str) -> String {
    let trimmed = text.trim();

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

    if let Some(result) = find_balanced_json(trimmed, b'{', b'}') {
        return result;
    }
    if let Some(result) = find_balanced_json(trimmed, b'[', b']') {
        return result;
    }

    trimmed.to_string()
}

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
            if b == b'u' && i + 4 < bytes.len() {
                i += 5;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_from_code_fence() {
        let text = "Here is the result:\n```json\n{\"key\": \"value\"}\n```\nDone.";
        assert_eq!(extract_json_from_text(text), "{\"key\": \"value\"}");
    }

    #[test]
    fn extract_from_code_fence_no_language() {
        let text = "```\n[1, 2, 3]\n```";
        assert_eq!(extract_json_from_text(text), "[1, 2, 3]");
    }

    #[test]
    fn extract_balanced_object() {
        let text = "prefix {\"a\": 1, \"b\": {\"c\": 2}} suffix";
        assert_eq!(
            extract_json_from_text(text),
            "{\"a\": 1, \"b\": {\"c\": 2}}"
        );
    }

    #[test]
    fn extract_balanced_array() {
        let text = "result: [1, [2, 3], 4] end";
        assert_eq!(extract_json_from_text(text), "[1, [2, 3], 4]");
    }

    #[test]
    fn extract_plain_text_fallback() {
        let text = "just plain text";
        assert_eq!(extract_json_from_text(text), "just plain text");
    }

    #[test]
    fn extract_respects_string_brackets() {
        let text = r#"{"msg": "hello {world} [test]"}"#;
        assert_eq!(extract_json_from_text(text), text);
    }

    #[test]
    fn extract_handles_escaped_quotes() {
        let text = r#"{"key": "value with \" quote"}"#;
        assert_eq!(extract_json_from_text(text), text);
    }
}
