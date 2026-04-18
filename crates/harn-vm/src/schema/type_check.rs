use std::collections::BTreeMap;

use crate::value::VmValue;

pub(super) fn schema_is_object_like(schema: &BTreeMap<String, VmValue>) -> bool {
    schema_type_name(schema) == Some("dict")
        || schema.contains_key("properties")
        || schema.contains_key("required")
        || schema.contains_key("additional_properties")
}

pub(super) fn schema_expected_label(schema: &BTreeMap<String, VmValue>) -> String {
    if schema_is_object_like(schema) {
        return "dict".to_string();
    }
    if let Some(expected_type) = schema_type_name(schema) {
        return expected_type.to_string();
    }
    if let Some(VmValue::List(branches)) = schema.get("union") {
        let labels = branches
            .iter()
            .filter_map(VmValue::as_dict)
            .map(schema_expected_label)
            .collect::<Vec<_>>();
        if !labels.is_empty() {
            return labels.join("|");
        }
    }
    "value".to_string()
}

pub(super) fn schema_type_name(schema: &BTreeMap<String, VmValue>) -> Option<&str> {
    match schema.get("type") {
        Some(VmValue::String(type_name)) => Some(type_name.as_ref()),
        _ => None,
    }
}

pub(super) fn value_matches_type(value: &VmValue, expected: &str, numeric_compat: bool) -> bool {
    match expected {
        "any" => true,
        "dict" => matches!(value, VmValue::Dict(_) | VmValue::StructInstance { .. }),
        "int" => {
            matches!(value, VmValue::Int(_))
                || (numeric_compat && matches!(value, VmValue::Float(_)))
        }
        "float" => {
            matches!(value, VmValue::Float(_))
                || (numeric_compat && matches!(value, VmValue::Int(_)))
        }
        other => actual_value_type(value) == other,
    }
}

pub(super) fn actual_value_type(value: &VmValue) -> &'static str {
    match value {
        VmValue::StructInstance { .. } => "dict",
        _ => value.type_name(),
    }
}
