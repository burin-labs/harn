use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::VmValue;

use super::type_check::schema_type_name;
use super::{schema_bool, schema_i64, schema_number, vm_value_to_serde_json};

pub(super) fn resolve_canonical_ref(
    root_schema: &BTreeMap<String, VmValue>,
    pointer: &str,
) -> Option<BTreeMap<String, VmValue>> {
    let stripped = pointer.trim_start_matches('#').trim_start_matches('/');
    if stripped.is_empty() {
        return Some(root_schema.clone());
    }

    let mut current = VmValue::Dict(Rc::new(root_schema.clone()));
    for segment in stripped.split('/') {
        let decoded = segment.replace("~1", "/").replace("~0", "~");
        current = match current {
            VmValue::Dict(ref map) => map.get(&decoded)?.clone(),
            VmValue::List(ref list) => {
                let idx = decoded.parse::<usize>().ok()?;
                list.get(idx)?.clone()
            }
            _ => return None,
        };
    }
    current.as_dict().cloned()
}

pub(super) fn canonicalize_schema_value(schema: &VmValue) -> Result<VmValue, String> {
    let schema_dict = schema
        .as_dict()
        .ok_or_else(|| "schema must be a dict".to_string())?;
    Ok(VmValue::Dict(Rc::new(canonicalize_schema_dict(
        schema_dict,
    )?)))
}

fn canonicalize_schema_dict(
    schema: &BTreeMap<String, VmValue>,
) -> Result<BTreeMap<String, VmValue>, String> {
    let mut out = BTreeMap::new();

    for key in [
        "title",
        "description",
        "format",
        "deprecated",
        "readOnly",
        "writeOnly",
        "example",
        "examples",
        "x-harn-type",
    ] {
        if let Some(value) = schema.get(key) {
            out.insert(key.to_string(), value.clone());
        }
    }

    if let Some(reference) = schema.get("$ref") {
        out.insert("$ref".to_string(), reference.clone());
    }

    if let Some(properties) = schema.get("properties").and_then(VmValue::as_dict) {
        let mut next = BTreeMap::new();
        for (key, value) in properties {
            next.insert(key.clone(), canonicalize_schema_value(value)?);
        }
        out.insert("properties".to_string(), VmValue::Dict(Rc::new(next)));
    }

    if let Some(items) = schema.get("items") {
        out.insert("items".to_string(), canonicalize_schema_value(items)?);
    }

    if let Some(additional) = schema
        .get("additional_properties")
        .or_else(|| schema.get("additionalProperties"))
    {
        match additional {
            VmValue::Bool(value) => {
                out.insert("additional_properties".to_string(), VmValue::Bool(*value));
            }
            VmValue::Dict(_) => {
                out.insert(
                    "additional_properties".to_string(),
                    canonicalize_schema_value(additional)?,
                );
            }
            _ => {}
        }
    }

    if let Some(required) = schema.get("required") {
        out.insert("required".to_string(), normalize_string_list(required));
    }

    if let Some(default) = schema.get("default") {
        out.insert("default".to_string(), default.clone());
    }
    if let Some(const_value) = schema.get("const") {
        out.insert("const".to_string(), const_value.clone());
    }
    if let Some(enum_values) = schema.get("enum") {
        out.insert("enum".to_string(), enum_values.clone());
    }

    if let Some(min) = schema.get("min").or_else(|| schema.get("minimum")) {
        out.insert("min".to_string(), min.clone());
    }
    if let Some(max) = schema.get("max").or_else(|| schema.get("maximum")) {
        out.insert("max".to_string(), max.clone());
    }
    if let Some(min_length) = schema.get("min_length").or_else(|| schema.get("minLength")) {
        out.insert("min_length".to_string(), min_length.clone());
    }
    if let Some(max_length) = schema.get("max_length").or_else(|| schema.get("maxLength")) {
        out.insert("max_length".to_string(), max_length.clone());
    }
    if let Some(min_items) = schema.get("min_items").or_else(|| schema.get("minItems")) {
        out.insert("min_items".to_string(), min_items.clone());
    }
    if let Some(max_items) = schema.get("max_items").or_else(|| schema.get("maxItems")) {
        out.insert("max_items".to_string(), max_items.clone());
    }
    if let Some(pattern) = schema.get("pattern") {
        out.insert("pattern".to_string(), pattern.clone());
    }

    if schema_bool(schema, "nullable") {
        out.insert("nullable".to_string(), VmValue::Bool(true));
    }

    if let Some(definitions) = schema.get("definitions").and_then(VmValue::as_dict) {
        out.insert(
            "definitions".to_string(),
            VmValue::Dict(Rc::new(canonicalize_schema_map(definitions)?)),
        );
    }

    if let Some(components) = schema.get("components").and_then(VmValue::as_dict) {
        let mut next_components = components.clone();
        if let Some(schemas) = components.get("schemas").and_then(VmValue::as_dict) {
            next_components.insert(
                "schemas".to_string(),
                VmValue::Dict(Rc::new(canonicalize_schema_map(schemas)?)),
            );
        }
        out.insert(
            "components".to_string(),
            VmValue::Dict(Rc::new(next_components)),
        );
    }

    if let Some(union) = schema
        .get("union")
        .or_else(|| schema.get("oneOf"))
        .or_else(|| schema.get("anyOf"))
    {
        out.insert("union".to_string(), canonicalize_schema_list(union)?);
    }

    if let Some(all_of) = schema.get("all_of").or_else(|| schema.get("allOf")) {
        out.insert("all_of".to_string(), canonicalize_schema_list(all_of)?);
    }

    match schema.get("type") {
        Some(VmValue::String(type_name)) => {
            let normalized_type = normalize_type_name(type_name);
            out.insert(
                "type".to_string(),
                VmValue::String(Rc::from(normalized_type.as_str())),
            );
        }
        Some(VmValue::List(type_names)) => {
            let union = type_names
                .iter()
                .map(|item| {
                    let type_name = normalize_type_name(&item.display());
                    let mut branch = BTreeMap::new();
                    branch.insert(
                        "type".to_string(),
                        VmValue::String(Rc::from(type_name.as_str())),
                    );
                    VmValue::Dict(Rc::new(branch))
                })
                .collect::<Vec<_>>();
            out.insert("union".to_string(), VmValue::List(Rc::new(union)));
        }
        _ => {}
    }

    if let Some(VmValue::Bool(true)) = schema.get("uniqueItems") {
        if schema_type_name(&out) == Some("list") && !out.contains_key("x-harn-type") {
            out.insert("x-harn-type".to_string(), VmValue::String(Rc::from("set")));
        }
    }

    if out.get("x-harn-type").map(|v| v.display()) == Some("set".to_string()) {
        out.insert("type".to_string(), VmValue::String(Rc::from("set")));
    }

    Ok(out)
}

fn canonicalize_schema_map(
    source: &BTreeMap<String, VmValue>,
) -> Result<BTreeMap<String, VmValue>, String> {
    let mut next = BTreeMap::new();
    for (key, value) in source {
        next.insert(key.clone(), canonicalize_schema_value(value)?);
    }
    Ok(next)
}

fn canonicalize_schema_list(value: &VmValue) -> Result<VmValue, String> {
    let list = match value {
        VmValue::List(list) => list,
        _ => return Err("schema union/all_of must be a list".to_string()),
    };
    Ok(VmValue::List(Rc::new(
        list.iter()
            .map(canonicalize_schema_value)
            .collect::<Result<Vec<_>, _>>()?,
    )))
}

fn normalize_string_list(value: &VmValue) -> VmValue {
    match value {
        VmValue::List(items) => VmValue::List(Rc::new(
            items
                .iter()
                .map(|item| VmValue::String(Rc::from(item.display())))
                .collect(),
        )),
        _ => VmValue::List(Rc::new(Vec::new())),
    }
}

fn normalize_type_name(type_name: &str) -> String {
    match type_name {
        "object" => "dict".to_string(),
        "array" => "list".to_string(),
        "integer" => "int".to_string(),
        "number" => "float".to_string(),
        "boolean" => "bool".to_string(),
        "null" => "nil".to_string(),
        other => other.to_string(),
    }
}

pub(super) fn canonical_to_json_schema(schema: &VmValue, openapi_style: bool) -> serde_json::Value {
    let schema_dict = match schema.as_dict() {
        Some(dict) => dict,
        None => return serde_json::json!({}),
    };
    let mut out = serde_json::Map::new();

    if let Some(reference) = schema_dict.get("$ref").and_then(|value| match value {
        VmValue::String(value) => Some(value.to_string()),
        _ => None,
    }) {
        out.insert("$ref".to_string(), serde_json::Value::String(reference));
    }

    if let Some(type_name) = schema_type_name(schema_dict) {
        match type_name {
            "set" => {
                out.insert(
                    "type".to_string(),
                    serde_json::Value::String("array".into()),
                );
                out.insert("uniqueItems".to_string(), serde_json::Value::Bool(true));
            }
            "closure" => {
                out.insert(
                    "type".to_string(),
                    serde_json::Value::String("string".into()),
                );
                out.insert(
                    "x-harn-type".to_string(),
                    serde_json::Value::String("closure".into()),
                );
            }
            "builtin" => {
                out.insert(
                    "type".to_string(),
                    serde_json::Value::String("string".into()),
                );
                out.insert(
                    "x-harn-type".to_string(),
                    serde_json::Value::String("builtin".into()),
                );
            }
            other => {
                out.insert("type".to_string(), json_type_for_harn(other));
            }
        }
    }

    if let Some(min) = schema_number(schema_dict, "min") {
        out.insert("minimum".to_string(), serde_json::json!(min));
    }
    if let Some(max) = schema_number(schema_dict, "max") {
        out.insert("maximum".to_string(), serde_json::json!(max));
    }
    if let Some(min_length) = schema_i64(schema_dict, "min_length") {
        out.insert("minLength".to_string(), serde_json::json!(min_length));
    }
    if let Some(max_length) = schema_i64(schema_dict, "max_length") {
        out.insert("maxLength".to_string(), serde_json::json!(max_length));
    }
    if let Some(min_items) = schema_i64(schema_dict, "min_items") {
        out.insert("minItems".to_string(), serde_json::json!(min_items));
    }
    if let Some(max_items) = schema_i64(schema_dict, "max_items") {
        out.insert("maxItems".to_string(), serde_json::json!(max_items));
    }
    if let Some(VmValue::String(pattern)) = schema_dict.get("pattern") {
        out.insert(
            "pattern".to_string(),
            serde_json::Value::String(pattern.to_string()),
        );
    }
    if let Some(default) = schema_dict.get("default") {
        out.insert("default".to_string(), vm_value_to_serde_json(default));
    }
    if let Some(const_value) = schema_dict.get("const") {
        out.insert("const".to_string(), vm_value_to_serde_json(const_value));
    }
    if let Some(VmValue::List(enum_values)) = schema_dict.get("enum") {
        out.insert(
            "enum".to_string(),
            serde_json::Value::Array(enum_values.iter().map(vm_value_to_serde_json).collect()),
        );
    }
    if let Some(VmValue::Dict(item_schema)) = schema_dict.get("items") {
        out.insert(
            "items".to_string(),
            canonical_to_json_schema(&VmValue::Dict(item_schema.clone()), openapi_style),
        );
    }
    if let Some(VmValue::Dict(properties)) = schema_dict.get("properties") {
        let mut props = serde_json::Map::new();
        for (name, child) in properties.iter() {
            props.insert(name.clone(), canonical_to_json_schema(child, openapi_style));
        }
        out.insert("properties".to_string(), serde_json::Value::Object(props));
    }
    if let Some(VmValue::List(required)) = schema_dict.get("required") {
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
    if let Some(extra) = schema_dict.get("additional_properties") {
        let exported = match extra {
            VmValue::Bool(value) => serde_json::Value::Bool(*value),
            VmValue::Dict(_) => canonical_to_json_schema(extra, openapi_style),
            _ => serde_json::Value::Bool(true),
        };
        out.insert("additionalProperties".to_string(), exported);
    }
    if let Some(VmValue::List(union_schemas)) = schema_dict.get("union") {
        let mut branches = union_schemas
            .iter()
            .map(|value| canonical_to_json_schema(value, openapi_style))
            .collect::<Vec<_>>();
        if openapi_style && branches.len() == 2 {
            let null_index = branches.iter().position(|value| value == "null");
            if let Some(null_index) = null_index {
                let other_index = if null_index == 0 { 1 } else { 0 };
                if let Some(other_type) = branches[other_index].as_object_mut() {
                    other_type.insert("nullable".to_string(), serde_json::Value::Bool(true));
                    return serde_json::Value::Object(other_type.clone());
                }
            }
        }
        out.insert("oneOf".to_string(), serde_json::Value::Array(branches));
    }
    if let Some(VmValue::List(all_of)) = schema_dict.get("all_of") {
        out.insert(
            "allOf".to_string(),
            serde_json::Value::Array(
                all_of
                    .iter()
                    .map(|value| canonical_to_json_schema(value, openapi_style))
                    .collect(),
            ),
        );
    }

    if schema_bool(schema_dict, "nullable") {
        if openapi_style {
            out.insert("nullable".to_string(), serde_json::Value::Bool(true));
        } else if let Some(existing) = out.remove("type") {
            out.insert(
                "type".to_string(),
                serde_json::Value::Array(vec![existing, serde_json::Value::String("null".into())]),
            );
        }
    }

    if let Some(VmValue::Dict(definitions)) = schema_dict.get("definitions") {
        out.insert(
            "definitions".to_string(),
            serde_json::Value::Object(
                definitions
                    .iter()
                    .map(|(name, child)| {
                        (name.clone(), canonical_to_json_schema(child, openapi_style))
                    })
                    .collect(),
            ),
        );
    }
    if let Some(VmValue::Dict(components)) = schema_dict.get("components") {
        let mut next_components = serde_json::Map::new();
        for (key, value) in components.iter() {
            if key == "schemas" {
                let schema_map = value.as_dict().cloned().unwrap_or_default();
                next_components.insert(
                    "schemas".to_string(),
                    serde_json::Value::Object(
                        schema_map
                            .iter()
                            .map(|(name, child)| {
                                (name.clone(), canonical_to_json_schema(child, openapi_style))
                            })
                            .collect(),
                    ),
                );
            } else {
                next_components.insert(key.clone(), vm_value_to_serde_json(value));
            }
        }
        out.insert(
            "components".to_string(),
            serde_json::Value::Object(next_components),
        );
    }

    for key in [
        "title",
        "description",
        "format",
        "deprecated",
        "readOnly",
        "writeOnly",
        "example",
        "examples",
        "x-harn-type",
    ] {
        if let Some(value) = schema_dict.get(key) {
            out.insert(key.to_string(), vm_value_to_serde_json(value));
        }
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

pub fn json_to_vm_value(jv: &serde_json::Value) -> VmValue {
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
