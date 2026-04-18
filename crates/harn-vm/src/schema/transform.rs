use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::VmValue;

pub(crate) fn merge_schema_dicts(
    base: &BTreeMap<String, VmValue>,
    overrides: &BTreeMap<String, VmValue>,
) -> BTreeMap<String, VmValue> {
    let mut merged = base.clone();
    for (key, value) in overrides {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

pub(crate) fn schema_partial_dict(schema: &BTreeMap<String, VmValue>) -> BTreeMap<String, VmValue> {
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
    if let Some(VmValue::List(branches)) = schema.get("union") {
        partial.insert(
            "union".to_string(),
            VmValue::List(Rc::new(
                branches
                    .iter()
                    .map(|branch| {
                        branch
                            .as_dict()
                            .map(|dict| VmValue::Dict(Rc::new(schema_partial_dict(dict))))
                            .unwrap_or_else(|| branch.clone())
                    })
                    .collect(),
            )),
        );
    }
    if let Some(VmValue::List(branches)) = schema.get("all_of") {
        partial.insert(
            "all_of".to_string(),
            VmValue::List(Rc::new(
                branches
                    .iter()
                    .map(|branch| {
                        branch
                            .as_dict()
                            .map(|dict| VmValue::Dict(Rc::new(schema_partial_dict(dict))))
                            .unwrap_or_else(|| branch.clone())
                    })
                    .collect(),
            )),
        );
    }
    if let Some(VmValue::Dict(item_schema)) = schema.get("items") {
        partial.insert(
            "items".to_string(),
            VmValue::Dict(Rc::new(schema_partial_dict(item_schema))),
        );
    }
    if let Some(VmValue::Dict(extra_schema)) = schema.get("additional_properties") {
        partial.insert(
            "additional_properties".to_string(),
            VmValue::Dict(Rc::new(schema_partial_dict(extra_schema))),
        );
    }
    partial
}

pub(crate) fn schema_pick_dict(
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

pub(crate) fn schema_omit_dict(
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
