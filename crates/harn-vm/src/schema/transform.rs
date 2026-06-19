use std::collections::BTreeMap;

use crate::value::VmValue;

pub(crate) fn merge_schema_dicts(
    base: &crate::value::DictMap,
    overrides: &crate::value::DictMap,
) -> crate::value::DictMap {
    let mut merged = base.clone();
    for (key, value) in overrides {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

pub(crate) fn schema_partial_dict(schema: &crate::value::DictMap) -> crate::value::DictMap {
    let mut partial = schema.clone();
    partial.remove("required");
    if let Some(VmValue::Dict(properties)) = schema.get("properties") {
        let mut next_props = BTreeMap::new();
        for (key, value) in properties.iter() {
            if let Some(child) = value.as_dict() {
                next_props.insert(key.clone(), VmValue::dict(schema_partial_dict(child)));
            } else {
                next_props.insert(key.clone(), value.clone());
            }
        }
        partial.insert(
            crate::value::intern_key("properties"),
            VmValue::dict(next_props),
        );
    }
    if let Some(VmValue::List(branches)) = schema.get("union") {
        partial.insert(
            crate::value::intern_key("union"),
            VmValue::List(std::sync::Arc::new(
                branches
                    .iter()
                    .map(|branch| {
                        branch
                            .as_dict()
                            .map(|dict| VmValue::dict(schema_partial_dict(dict)))
                            .unwrap_or_else(|| branch.clone())
                    })
                    .collect(),
            )),
        );
    }
    if let Some(VmValue::List(branches)) = schema.get("all_of") {
        partial.insert(
            crate::value::intern_key("all_of"),
            VmValue::List(std::sync::Arc::new(
                branches
                    .iter()
                    .map(|branch| {
                        branch
                            .as_dict()
                            .map(|dict| VmValue::dict(schema_partial_dict(dict)))
                            .unwrap_or_else(|| branch.clone())
                    })
                    .collect(),
            )),
        );
    }
    if let Some(VmValue::Dict(item_schema)) = schema.get("items") {
        partial.insert(
            crate::value::intern_key("items"),
            VmValue::dict(schema_partial_dict(item_schema)),
        );
    }
    if let Some(VmValue::Dict(extra_schema)) = schema.get("additional_properties") {
        partial.insert(
            crate::value::intern_key("additional_properties"),
            VmValue::dict(schema_partial_dict(extra_schema)),
        );
    }
    partial
}

pub(crate) fn schema_pick_dict(
    schema: &crate::value::DictMap,
    keys: &[String],
) -> crate::value::DictMap {
    let mut picked = schema.clone();
    if let Some(VmValue::Dict(properties)) = schema.get("properties") {
        let filtered: crate::value::DictMap = properties
            .iter()
            .filter(|(key, _)| keys.iter().any(|k| k.as_str() == key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        picked.insert(
            crate::value::intern_key("properties"),
            VmValue::dict(filtered),
        );
    }
    if let Some(VmValue::List(required)) = schema.get("required") {
        picked.insert(
            crate::value::intern_key("required"),
            VmValue::List(std::sync::Arc::new(
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

pub(crate) fn schema_omit_dict(
    schema: &crate::value::DictMap,
    keys: &[String],
) -> crate::value::DictMap {
    let mut kept = schema.clone();
    if let Some(VmValue::Dict(properties)) = schema.get("properties") {
        let filtered: crate::value::DictMap = properties
            .iter()
            .filter(|(key, _)| !keys.iter().any(|k| k.as_str() == key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        kept.insert(
            crate::value::intern_key("properties"),
            VmValue::dict(filtered),
        );
    }
    if let Some(VmValue::List(required)) = schema.get("required") {
        kept.insert(
            crate::value::intern_key("required"),
            VmValue::List(std::sync::Arc::new(
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
