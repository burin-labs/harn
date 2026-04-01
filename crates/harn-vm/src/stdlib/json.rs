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

    vm.register_builtin("schema_check", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "schema_check requires 2 arguments: data and schema",
            ))));
        }
        Ok(schema_result_value(&args[0], &args[1], false))
    });

    vm.register_builtin("schema_parse", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "schema_parse requires 2 arguments: data and schema",
            ))));
        }
        Ok(schema_result_value(&args[0], &args[1], true))
    });

    vm.register_builtin("schema_to_json_schema", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "schema_to_json_schema requires 1 argument: schema",
            ))));
        }
        let schema = args[0].as_dict().ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "schema_to_json_schema: schema must be a dict",
            )))
        })?;
        Ok(json_to_vm_value(&schema_dict_to_json_schema(schema)))
    });

    vm.register_builtin("schema_extend", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "schema_extend requires 2 arguments: base and overrides",
            ))));
        }
        let base = args[0].as_dict().ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "schema_extend: base must be a dict",
            )))
        })?;
        let overrides = args[1].as_dict().ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "schema_extend: overrides must be a dict",
            )))
        })?;
        Ok(VmValue::Dict(Rc::new(merge_schema_dicts(base, overrides))))
    });

    vm.register_builtin("schema_partial", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "schema_partial requires 1 argument: schema",
            ))));
        }
        let schema = args[0].as_dict().ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "schema_partial: schema must be a dict",
            )))
        })?;
        Ok(VmValue::Dict(Rc::new(schema_partial_dict(schema))))
    });

    vm.register_builtin("schema_pick", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "schema_pick requires 2 arguments: schema and keys",
            ))));
        }
        let schema = args[0].as_dict().ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "schema_pick: schema must be a dict",
            )))
        })?;
        let keys = schema_key_list(&args[1], "schema_pick")?;
        Ok(VmValue::Dict(Rc::new(schema_pick_dict(schema, &keys))))
    });

    vm.register_builtin("schema_omit", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "schema_omit requires 2 arguments: schema and keys",
            ))));
        }
        let schema = args[0].as_dict().ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "schema_omit: schema must be a dict",
            )))
        })?;
        let keys = schema_key_list(&args[1], "schema_omit")?;
        Ok(VmValue::Dict(Rc::new(schema_omit_dict(schema, &keys))))
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
    let result = validate_against_schema(value, schema, path, false);
    errors.extend(result.errors);
}

struct ValidationResult {
    value: VmValue,
    errors: Vec<String>,
}

pub(crate) fn schema_result_value(
    data: &VmValue,
    schema: &VmValue,
    apply_defaults: bool,
) -> VmValue {
    let schema_dict = match schema.as_dict() {
        Some(dict) => dict,
        None => return result_err_value(vec!["schema must be a dict".to_string()], None),
    };
    let result = validate_against_schema(data, schema_dict, "", apply_defaults);
    if result.errors.is_empty() {
        result_ok_value(result.value)
    } else {
        result_err_value(result.errors, Some(result.value))
    }
}

fn result_ok_value(value: VmValue) -> VmValue {
    VmValue::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Ok".to_string(),
        fields: vec![value],
    }
}

fn result_err_value(errors: Vec<String>, value: Option<VmValue>) -> VmValue {
    let mut payload = BTreeMap::new();
    payload.insert(
        "message".to_string(),
        VmValue::String(Rc::from(
            errors
                .first()
                .cloned()
                .unwrap_or_else(|| "schema validation failed".to_string()),
        )),
    );
    payload.insert(
        "errors".to_string(),
        VmValue::List(Rc::new(
            errors
                .into_iter()
                .map(|err| VmValue::String(Rc::from(err)))
                .collect(),
        )),
    );
    if let Some(value) = value {
        payload.insert("value".to_string(), value);
    }
    VmValue::EnumVariant {
        enum_name: "Result".to_string(),
        variant: "Err".to_string(),
        fields: vec![VmValue::Dict(Rc::new(payload))],
    }
}

fn validate_against_schema(
    value: &VmValue,
    schema: &BTreeMap<String, VmValue>,
    path: &str,
    apply_defaults: bool,
) -> ValidationResult {
    if matches!(value, VmValue::Nil) && schema_bool(schema, "nullable") {
        return ValidationResult {
            value: VmValue::Nil,
            errors: Vec::new(),
        };
    }

    if let Some(VmValue::List(union_schemas)) = schema.get("union") {
        for branch in union_schemas.iter() {
            if let Some(dict) = branch.as_dict() {
                let branch_result = validate_against_schema(value, dict, path, apply_defaults);
                if branch_result.errors.is_empty() {
                    return branch_result;
                }
            }
        }
        return ValidationResult {
            value: value.clone(),
            errors: vec![format!(
                "at {}: value did not match any union branch",
                location_label(path)
            )],
        };
    }

    if let Some(VmValue::String(expected_type)) = schema.get("type") {
        let actual_type = value.type_name();
        let type_str: &str = expected_type;
        if type_str != "any" && actual_type != type_str {
            return ValidationResult {
                value: value.clone(),
                errors: vec![format!(
                    "at {}: expected type '{}', got '{}'",
                    location_label(path),
                    type_str,
                    actual_type
                )],
            };
        }
    }

    let mut errors = Vec::new();
    let mut normalized = value.clone();

    match value {
        VmValue::Dict(map) => {
            if let Some(VmValue::List(required_keys)) = schema.get("required") {
                for key_val in required_keys.iter() {
                    let key = key_val.display();
                    if !map.contains_key(&key) {
                        let has_default = schema
                            .get("properties")
                            .and_then(VmValue::as_dict)
                            .and_then(|props| props.get(&key))
                            .and_then(VmValue::as_dict)
                            .is_some_and(|prop_schema| prop_schema.contains_key("default"));
                        if apply_defaults && has_default {
                            continue;
                        }
                        errors.push(format!(
                            "at {}: missing required key '{}'",
                            location_label(path),
                            key
                        ));
                    }
                }
            }

            let mut merged = (**map).clone();
            if let Some(VmValue::Dict(prop_schemas)) = schema.get("properties") {
                for (key, prop_schema) in prop_schemas.iter() {
                    let Some(prop_schema_dict) = prop_schema.as_dict() else {
                        continue;
                    };
                    let child_path = child_path(path, key);
                    match map.get(key) {
                        Some(prop_value) => {
                            let child = validate_against_schema(
                                prop_value,
                                prop_schema_dict,
                                &child_path,
                                apply_defaults,
                            );
                            if child.errors.is_empty() {
                                merged.insert(key.clone(), child.value);
                            } else {
                                errors.extend(child.errors);
                            }
                        }
                        None if apply_defaults => {
                            if let Some(default_value) = prop_schema_dict.get("default") {
                                let child = validate_against_schema(
                                    default_value,
                                    prop_schema_dict,
                                    &child_path,
                                    apply_defaults,
                                );
                                if child.errors.is_empty() {
                                    merged.insert(key.clone(), child.value);
                                } else {
                                    errors.extend(child.errors);
                                }
                            }
                        }
                        None => {}
                    }
                }
            }
            normalized = VmValue::Dict(Rc::new(merged));
        }
        VmValue::List(items) => {
            if let Some(min_items) = schema_i64(schema, "min_items") {
                if (items.len() as i64) < min_items {
                    errors.push(format!(
                        "at {}: expected at least {} items, got {}",
                        location_label(path),
                        min_items,
                        items.len()
                    ));
                }
            }
            if let Some(max_items) = schema_i64(schema, "max_items") {
                if (items.len() as i64) > max_items {
                    errors.push(format!(
                        "at {}: expected at most {} items, got {}",
                        location_label(path),
                        max_items,
                        items.len()
                    ));
                }
            }
            if let Some(VmValue::Dict(item_schema)) = schema.get("items") {
                let mut normalized_items = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    let child = validate_against_schema(
                        item,
                        item_schema,
                        &index_path(path, i),
                        apply_defaults,
                    );
                    if child.errors.is_empty() {
                        normalized_items.push(child.value);
                    } else {
                        errors.extend(child.errors);
                    }
                }
                normalized = VmValue::List(Rc::new(normalized_items));
            }
        }
        VmValue::String(text) => {
            let length = text.chars().count() as i64;
            if let Some(min_length) = schema_i64(schema, "min_length") {
                if length < min_length {
                    errors.push(format!(
                        "at {}: expected length >= {}, got {}",
                        location_label(path),
                        min_length,
                        length
                    ));
                }
            }
            if let Some(max_length) = schema_i64(schema, "max_length") {
                if length > max_length {
                    errors.push(format!(
                        "at {}: expected length <= {}, got {}",
                        location_label(path),
                        max_length,
                        length
                    ));
                }
            }
            if let Some(VmValue::String(pattern)) = schema.get("pattern") {
                match regex::Regex::new(pattern) {
                    Ok(re) => {
                        if !re.is_match(text) {
                            errors.push(format!(
                                "at {}: value does not match pattern '{}'",
                                location_label(path),
                                pattern
                            ));
                        }
                    }
                    Err(error) => errors.push(format!(
                        "at {}: invalid regex pattern '{}': {}",
                        location_label(path),
                        pattern,
                        error
                    )),
                }
            }
            if let Some(VmValue::List(enum_values)) = schema.get("enum") {
                if !enum_values
                    .iter()
                    .any(|candidate| candidate.display() == value.display())
                {
                    errors.push(format!(
                        "at {}: value must be one of [{}]",
                        location_label(path),
                        enum_values
                            .iter()
                            .map(VmValue::display)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
        VmValue::Int(number) => {
            validate_numeric_constraints(*number as f64, schema, path, &mut errors);
        }
        VmValue::Float(number) => {
            validate_numeric_constraints(*number, schema, path, &mut errors);
        }
        _ => {}
    }

    ValidationResult {
        value: normalized,
        errors,
    }
}

fn validate_numeric_constraints(
    value: f64,
    schema: &BTreeMap<String, VmValue>,
    path: &str,
    errors: &mut Vec<String>,
) {
    if let Some(min) = schema_number(schema, "min") {
        if value < min {
            errors.push(format!(
                "at {}: expected value >= {}, got {}",
                location_label(path),
                min,
                value
            ));
        }
    }
    if let Some(max) = schema_number(schema, "max") {
        if value > max {
            errors.push(format!(
                "at {}: expected value <= {}, got {}",
                location_label(path),
                max,
                value
            ));
        }
    }
}

fn schema_dict_to_json_schema(schema: &BTreeMap<String, VmValue>) -> serde_json::Value {
    let mut out = serde_json::Map::new();

    if let Some(VmValue::String(type_name)) = schema.get("type") {
        out.insert("type".to_string(), json_type_for_harn(type_name));
    }
    if schema_bool(schema, "nullable") {
        if let Some(existing) = out.remove("type") {
            out.insert(
                "type".to_string(),
                serde_json::Value::Array(vec![existing, serde_json::Value::String("null".into())]),
            );
        }
    }
    if let Some(min) = schema_number(schema, "min") {
        out.insert("minimum".to_string(), serde_json::json!(min));
    }
    if let Some(max) = schema_number(schema, "max") {
        out.insert("maximum".to_string(), serde_json::json!(max));
    }
    if let Some(min_length) = schema_i64(schema, "min_length") {
        out.insert("minLength".to_string(), serde_json::json!(min_length));
    }
    if let Some(max_length) = schema_i64(schema, "max_length") {
        out.insert("maxLength".to_string(), serde_json::json!(max_length));
    }
    if let Some(VmValue::String(pattern)) = schema.get("pattern") {
        out.insert(
            "pattern".to_string(),
            serde_json::Value::String(pattern.to_string()),
        );
    }
    if let Some(VmValue::List(enum_values)) = schema.get("enum") {
        out.insert(
            "enum".to_string(),
            serde_json::Value::Array(enum_values.iter().map(vm_value_to_serde_json).collect()),
        );
    }
    if let Some(min_items) = schema_i64(schema, "min_items") {
        out.insert("minItems".to_string(), serde_json::json!(min_items));
    }
    if let Some(max_items) = schema_i64(schema, "max_items") {
        out.insert("maxItems".to_string(), serde_json::json!(max_items));
    }
    if let Some(VmValue::Dict(item_schema)) = schema.get("items") {
        out.insert("items".to_string(), schema_dict_to_json_schema(item_schema));
    }
    if let Some(VmValue::Dict(properties)) = schema.get("properties") {
        let mut props = serde_json::Map::new();
        for (name, child) in properties.iter() {
            if let Some(child_dict) = child.as_dict() {
                props.insert(name.clone(), schema_dict_to_json_schema(child_dict));
            }
        }
        out.insert("properties".to_string(), serde_json::Value::Object(props));
    }
    if let Some(VmValue::List(required)) = schema.get("required") {
        out.insert(
            "required".to_string(),
            serde_json::Value::Array(
                required
                    .iter()
                    .map(|value| serde_json::Value::String(value.display()))
                    .collect(),
            ),
        );
    }
    if let Some(VmValue::List(union_schemas)) = schema.get("union") {
        out.insert(
            "oneOf".to_string(),
            serde_json::Value::Array(
                union_schemas
                    .iter()
                    .filter_map(|value| value.as_dict().map(schema_dict_to_json_schema))
                    .collect(),
            ),
        );
    }
    if let Some(default) = schema.get("default") {
        out.insert("default".to_string(), vm_value_to_serde_json(default));
    }

    serde_json::Value::Object(out)
}

fn json_type_for_harn(type_name: &str) -> serde_json::Value {
    let json_type = match type_name {
        "int" => "integer",
        "float" => "number",
        "bool" => "boolean",
        "list" => "array",
        "dict" => "object",
        "nil" => "null",
        other => other,
    };
    serde_json::Value::String(json_type.to_string())
}

fn vm_value_to_serde_json(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(value) => serde_json::Value::Bool(*value),
        VmValue::Int(value) => serde_json::json!(value),
        VmValue::Float(value) => serde_json::json!(value),
        VmValue::String(value) => serde_json::Value::String(value.to_string()),
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_serde_json).collect())
        }
        VmValue::Dict(items) => serde_json::Value::Object(
            items
                .iter()
                .map(|(key, value)| (key.clone(), vm_value_to_serde_json(value)))
                .collect(),
        ),
        _ => serde_json::Value::String(value.display()),
    }
}

fn schema_bool(schema: &BTreeMap<String, VmValue>, key: &str) -> bool {
    matches!(schema.get(key), Some(VmValue::Bool(true)))
}

fn schema_i64(schema: &BTreeMap<String, VmValue>, key: &str) -> Option<i64> {
    match schema.get(key) {
        Some(VmValue::Int(value)) => Some(*value),
        _ => None,
    }
}

fn schema_number(schema: &BTreeMap<String, VmValue>, key: &str) -> Option<f64> {
    match schema.get(key) {
        Some(VmValue::Int(value)) => Some(*value as f64),
        Some(VmValue::Float(value)) => Some(*value),
        _ => None,
    }
}

fn location_label(path: &str) -> String {
    if path.is_empty() {
        "root".to_string()
    } else {
        path.to_string()
    }
}

fn child_path(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{}.{}", path, key)
    }
}

fn index_path(path: &str, index: usize) -> String {
    if path.is_empty() {
        format!("[{}]", index)
    } else {
        format!("{}[{}]", path, index)
    }
}

fn merge_schema_dicts(
    base: &BTreeMap<String, VmValue>,
    overrides: &BTreeMap<String, VmValue>,
) -> BTreeMap<String, VmValue> {
    let mut merged = base.clone();
    for (key, value) in overrides {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

fn schema_partial_dict(schema: &BTreeMap<String, VmValue>) -> BTreeMap<String, VmValue> {
    let mut partial = schema.clone();
    partial.remove("required");
    if let Some(VmValue::Dict(properties)) = schema.get("properties") {
        let mut next_props = BTreeMap::new();
        for (key, value) in properties.iter() {
            if let Some(child) = value.as_dict() {
                next_props.insert(
                    key.clone(),
                    VmValue::Dict(Rc::new(schema_partial_dict(child))),
                );
            } else {
                next_props.insert(key.clone(), value.clone());
            }
        }
        partial.insert("properties".to_string(), VmValue::Dict(Rc::new(next_props)));
    }
    partial
}

fn schema_pick_dict(
    schema: &BTreeMap<String, VmValue>,
    keys: &[String],
) -> BTreeMap<String, VmValue> {
    let mut picked = schema.clone();
    if let Some(VmValue::Dict(properties)) = schema.get("properties") {
        let filtered: BTreeMap<String, VmValue> = properties
            .iter()
            .filter(|(key, _)| keys.contains(*key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        picked.insert("properties".to_string(), VmValue::Dict(Rc::new(filtered)));
    }
    if let Some(VmValue::List(required)) = schema.get("required") {
        picked.insert(
            "required".to_string(),
            VmValue::List(Rc::new(
                required
                    .iter()
                    .filter(|value| keys.contains(&value.display()))
                    .cloned()
                    .collect(),
            )),
        );
    }
    picked
}

fn schema_omit_dict(
    schema: &BTreeMap<String, VmValue>,
    keys: &[String],
) -> BTreeMap<String, VmValue> {
    let mut kept = schema.clone();
    if let Some(VmValue::Dict(properties)) = schema.get("properties") {
        let filtered: BTreeMap<String, VmValue> = properties
            .iter()
            .filter(|(key, _)| !keys.contains(*key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        kept.insert("properties".to_string(), VmValue::Dict(Rc::new(filtered)));
    }
    if let Some(VmValue::List(required)) = schema.get("required") {
        kept.insert(
            "required".to_string(),
            VmValue::List(Rc::new(
                required
                    .iter()
                    .filter(|value| !keys.contains(&value.display()))
                    .cloned()
                    .collect(),
            )),
        );
    }
    kept
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
