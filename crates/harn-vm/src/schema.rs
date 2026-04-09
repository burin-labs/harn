use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{values_equal, VmError, VmValue};

#[derive(Clone, Copy, Debug)]
struct ValidationOptions {
    apply_defaults: bool,
    numeric_compat: bool,
}

#[derive(Debug, Clone)]
struct ValidationResult {
    value: VmValue,
    errors: Vec<String>,
}

pub(crate) fn schema_result_value(
    data: &VmValue,
    schema: &VmValue,
    apply_defaults: bool,
) -> VmValue {
    let normalized = match canonicalize_schema_value(schema) {
        Ok(schema) => schema,
        Err(error) => return result_err_value(vec![error], None),
    };
    let result = validate_schema_value(
        data,
        &normalized,
        ValidationOptions {
            apply_defaults,
            numeric_compat: false,
        },
    );
    if result.errors.is_empty() {
        result_ok_value(result.value)
    } else {
        result_err_value(result.errors, Some(result.value))
    }
}

pub(crate) fn schema_is_value(data: &VmValue, schema: &VmValue) -> Result<bool, VmError> {
    let normalized = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))?;
    Ok(validate_schema_value(
        data,
        &normalized,
        ValidationOptions {
            apply_defaults: false,
            numeric_compat: false,
        },
    )
    .errors
    .is_empty())
}

pub(crate) fn schema_expect_value(
    data: &VmValue,
    schema: &VmValue,
    apply_defaults: bool,
) -> Result<VmValue, VmError> {
    let normalized = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))?;
    let result = validate_schema_value(
        data,
        &normalized,
        ValidationOptions {
            apply_defaults,
            numeric_compat: false,
        },
    );
    if result.errors.is_empty() {
        Ok(result.value)
    } else {
        Err(VmError::Thrown(VmValue::String(Rc::from(
            result.errors.join("; "),
        ))))
    }
}

pub(crate) fn schema_assert_param(
    value: &VmValue,
    param_name: &str,
    schema: &VmValue,
) -> Result<(), VmError> {
    let normalized = canonicalize_schema_value(schema)
        .map_err(|error| VmError::TypeError(format!("parameter '{param_name}': {error}")))?;
    let schema_dict = normalized.as_dict().ok_or_else(|| {
        VmError::TypeError(format!("parameter '{param_name}': schema must be a dict"))
    })?;
    let options = ValidationOptions {
        apply_defaults: false,
        numeric_compat: true,
    };
    if let Some(error) =
        first_param_validation_error(value, schema_dict, schema_dict, param_name, options)
    {
        return if schema_is_object_like(schema_dict) {
            Err(VmError::TypeError(error))
        } else {
            Err(VmError::Runtime(format!("TypeError: {error}")))
        };
    }
    Ok(())
}

pub(crate) fn schema_to_json_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    let normalized = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))?;
    Ok(json_to_vm_value(&canonical_to_json_schema(
        &normalized,
        false,
    )))
}

pub(crate) fn schema_to_openapi_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    let normalized = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))?;
    Ok(json_to_vm_value(&canonical_to_json_schema(
        &normalized,
        true,
    )))
}

pub(crate) fn schema_from_json_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))
}

pub(crate) fn schema_from_openapi_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))
}

pub(crate) fn schema_extend_value(base: &VmValue, overrides: &VmValue) -> Result<VmValue, VmError> {
    let base = canonicalize_schema_value(base)
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))?;
    let overrides = canonicalize_schema_value(overrides)
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))?;
    let base_dict = base.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(Rc::from(
            "schema_extend: schema must be a dict",
        )))
    })?;
    let overrides_dict = overrides.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(Rc::from(
            "schema_extend: schema must be a dict",
        )))
    })?;
    Ok(VmValue::Dict(Rc::new(merge_schema_dicts(
        base_dict,
        overrides_dict,
    ))))
}

pub(crate) fn schema_partial_value(schema: &VmValue) -> Result<VmValue, VmError> {
    let schema = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))?;
    let schema_dict = schema.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(Rc::from(
            "schema_partial: schema must be a dict",
        )))
    })?;
    Ok(VmValue::Dict(Rc::new(schema_partial_dict(schema_dict))))
}

pub(crate) fn schema_pick_value(schema: &VmValue, keys: &[String]) -> Result<VmValue, VmError> {
    let schema = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))?;
    let schema_dict = schema.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(Rc::from(
            "schema_pick: schema must be a dict",
        )))
    })?;
    Ok(VmValue::Dict(Rc::new(schema_pick_dict(schema_dict, keys))))
}

pub(crate) fn schema_omit_value(schema: &VmValue, keys: &[String]) -> Result<VmValue, VmError> {
    let schema = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))?;
    let schema_dict = schema.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(Rc::from(
            "schema_omit: schema must be a dict",
        )))
    })?;
    Ok(VmValue::Dict(Rc::new(schema_omit_dict(schema_dict, keys))))
}

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

fn validate_schema_value(
    data: &VmValue,
    schema: &VmValue,
    options: ValidationOptions,
) -> ValidationResult {
    let root = schema.as_dict().cloned().unwrap_or_default();
    let schema_dict = schema.as_dict().cloned().unwrap_or_default();
    validate_against_schema(data, &schema_dict, &root, "", options)
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
    root_schema: &BTreeMap<String, VmValue>,
    path: &str,
    options: ValidationOptions,
) -> ValidationResult {
    if let Some(VmValue::String(pointer)) = schema.get("$ref") {
        match resolve_canonical_ref(root_schema, pointer) {
            Some(resolved) => {
                return validate_against_schema(value, &resolved, root_schema, path, options);
            }
            None => {
                return ValidationResult {
                    value: value.clone(),
                    errors: vec![format!(
                        "at {}: unresolved schema reference '{}'",
                        location_label(path),
                        pointer
                    )],
                };
            }
        }
    }

    if matches!(value, VmValue::Nil) && schema_bool(schema, "nullable") {
        return ValidationResult {
            value: VmValue::Nil,
            errors: Vec::new(),
        };
    }

    if let Some(const_value) = schema.get("const") {
        if !values_equal(value, const_value) {
            return ValidationResult {
                value: value.clone(),
                errors: vec![format!(
                    "at {}: expected constant {}, got {}",
                    location_label(path),
                    const_value.display(),
                    value.display()
                )],
            };
        }
    }

    if let Some(VmValue::List(branches)) = schema.get("all_of") {
        let mut normalized = value.clone();
        let mut errors = Vec::new();
        for branch in branches.iter() {
            let Some(branch_dict) = branch.as_dict() else {
                continue;
            };
            let branch_result =
                validate_against_schema(&normalized, branch_dict, root_schema, path, options);
            normalized = branch_result.value;
            errors.extend(branch_result.errors);
        }
        return ValidationResult {
            value: normalized,
            errors,
        };
    }

    if let Some(VmValue::List(union_schemas)) = schema.get("union") {
        for branch in union_schemas.iter() {
            if let Some(dict) = branch.as_dict() {
                let branch_result =
                    validate_against_schema(value, dict, root_schema, path, options);
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

    if let Some(expected_type) = schema_type_name(schema) {
        if !value_matches_type(value, expected_type, options.numeric_compat) {
            return ValidationResult {
                value: value.clone(),
                errors: vec![format!(
                    "at {}: expected type '{}', got '{}'",
                    location_label(path),
                    expected_type,
                    actual_value_type(value)
                )],
            };
        }
    }

    let mut errors = Vec::new();
    let mut normalized = value.clone();

    match value {
        VmValue::Dict(map) => {
            let (next_value, next_errors) =
                validate_object_fields(map, None, schema, root_schema, path, options);
            normalized = next_value;
            errors.extend(next_errors);
        }
        VmValue::StructInstance {
            struct_name,
            fields,
        } => {
            let (next_value, next_errors) = validate_object_fields(
                fields,
                Some(struct_name),
                schema,
                root_schema,
                path,
                options,
            );
            normalized = next_value;
            errors.extend(next_errors);
        }
        VmValue::List(items) | VmValue::Set(items) => {
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
                        root_schema,
                        &index_path(path, i),
                        options,
                    );
                    if child.errors.is_empty() {
                        normalized_items.push(child.value);
                    } else {
                        errors.extend(child.errors);
                    }
                }
                normalized = match value {
                    VmValue::Set(_) => VmValue::Set(Rc::new(normalized_items)),
                    _ => VmValue::List(Rc::new(normalized_items)),
                };
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
                    .any(|candidate| values_equal(candidate, value))
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
            validate_enum_membership(value, schema, path, &mut errors);
        }
        VmValue::Float(number) => {
            validate_numeric_constraints(*number, schema, path, &mut errors);
            validate_enum_membership(value, schema, path, &mut errors);
        }
        VmValue::Bool(_) | VmValue::Nil => {
            validate_enum_membership(value, schema, path, &mut errors);
        }
        _ => {}
    }

    ValidationResult {
        value: normalized,
        errors,
    }
}

fn validate_object_fields(
    fields: &BTreeMap<String, VmValue>,
    struct_name: Option<&str>,
    schema: &BTreeMap<String, VmValue>,
    root_schema: &BTreeMap<String, VmValue>,
    path: &str,
    options: ValidationOptions,
) -> (VmValue, Vec<String>) {
    let mut errors = Vec::new();
    let mut merged = fields.clone();
    let mut known_keys = std::collections::BTreeSet::new();

    if let Some(VmValue::List(required_keys)) = schema.get("required") {
        for key_val in required_keys.iter() {
            let key = key_val.display();
            if !fields.contains_key(&key) {
                let has_default = schema
                    .get("properties")
                    .and_then(VmValue::as_dict)
                    .and_then(|props| props.get(&key))
                    .and_then(VmValue::as_dict)
                    .is_some_and(|prop_schema| prop_schema.contains_key("default"));
                if options.apply_defaults && has_default {
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

    if let Some(VmValue::Dict(prop_schemas)) = schema.get("properties") {
        for (key, prop_schema) in prop_schemas.iter() {
            known_keys.insert(key.clone());
            let Some(prop_schema_dict) = prop_schema.as_dict() else {
                continue;
            };
            let child_path = child_path(path, key);
            match fields.get(key) {
                Some(prop_value) => {
                    let child = validate_against_schema(
                        prop_value,
                        prop_schema_dict,
                        root_schema,
                        &child_path,
                        options,
                    );
                    if child.errors.is_empty() {
                        merged.insert(key.clone(), child.value);
                    } else {
                        errors.extend(child.errors);
                    }
                }
                None if options.apply_defaults => {
                    if let Some(default_value) = prop_schema_dict.get("default") {
                        let child = validate_against_schema(
                            default_value,
                            prop_schema_dict,
                            root_schema,
                            &child_path,
                            options,
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

    match schema.get("additional_properties") {
        Some(VmValue::Bool(false)) => {
            for key in fields.keys() {
                if !known_keys.contains(key) {
                    errors.push(format!(
                        "at {}: unexpected key '{}'",
                        location_label(path),
                        key
                    ));
                }
            }
        }
        Some(VmValue::Dict(extra_schema)) => {
            for (key, value) in fields.iter() {
                if known_keys.contains(key) {
                    continue;
                }
                let child = validate_against_schema(
                    value,
                    extra_schema,
                    root_schema,
                    &child_path(path, key),
                    options,
                );
                if child.errors.is_empty() {
                    merged.insert(key.clone(), child.value);
                } else {
                    errors.extend(child.errors);
                }
            }
        }
        _ => {}
    }

    let normalized = if let Some(struct_name) = struct_name {
        VmValue::StructInstance {
            struct_name: struct_name.to_string(),
            fields: merged,
        }
    } else {
        VmValue::Dict(Rc::new(merged))
    };

    (normalized, errors)
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

fn validate_enum_membership(
    value: &VmValue,
    schema: &BTreeMap<String, VmValue>,
    path: &str,
    errors: &mut Vec<String>,
) {
    if let Some(VmValue::List(enum_values)) = schema.get("enum") {
        if !enum_values
            .iter()
            .any(|candidate| values_equal(candidate, value))
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

fn first_param_validation_error(
    value: &VmValue,
    schema: &BTreeMap<String, VmValue>,
    root_schema: &BTreeMap<String, VmValue>,
    param_name: &str,
    options: ValidationOptions,
) -> Option<String> {
    if let Some(VmValue::String(pointer)) = schema.get("$ref") {
        let resolved = resolve_canonical_ref(root_schema, pointer)?;
        return first_param_validation_error(value, &resolved, root_schema, param_name, options);
    }

    if matches!(value, VmValue::Nil) && schema_bool(schema, "nullable") {
        return None;
    }

    if let Some(VmValue::List(branches)) = schema.get("all_of") {
        for branch in branches.iter() {
            let branch_dict = branch.as_dict()?;
            if let Some(error) =
                first_param_validation_error(value, branch_dict, root_schema, param_name, options)
            {
                return Some(error);
            }
        }
        return None;
    }

    if let Some(VmValue::List(branches)) = schema.get("union") {
        for branch in branches.iter() {
            let branch_dict = branch.as_dict()?;
            first_param_validation_error(value, branch_dict, root_schema, param_name, options)?;
        }
        return Some(format!(
            "parameter '{}' expected {}, got {} ({})",
            param_name,
            schema_expected_label(schema),
            actual_value_type(value),
            value.display()
        ));
    }

    if schema_is_object_like(schema) {
        let fields = match value {
            VmValue::Dict(map) => Some(map.as_ref()),
            VmValue::StructInstance { fields, .. } => Some(fields),
            _ => None,
        };
        let fields = match fields {
            Some(fields) => fields,
            None => {
                return Some(format!(
                    "parameter '{}': expected dict or struct, got {}",
                    param_name,
                    value.type_name()
                ));
            }
        };
        return first_object_param_error(fields, schema, root_schema, param_name, options);
    }

    if let Some(expected_type) = schema_type_name(schema) {
        if !value_matches_type(value, expected_type, options.numeric_compat) {
            return Some(format!(
                "parameter '{}' expected {}, got {} ({})",
                param_name,
                expected_type,
                actual_value_type(value),
                value.display()
            ));
        }
    }

    let result = validate_against_schema(value, schema, root_schema, "root", options);
    if result.errors.is_empty() {
        None
    } else {
        let joined = result
            .errors
            .into_iter()
            .map(|error| {
                if error.starts_with("at root: ") {
                    error.replacen("at root: ", "", 1)
                } else {
                    error
                }
            })
            .collect::<Vec<_>>()
            .join("; ");
        Some(format!("parameter '{}': {}", param_name, joined))
    }
}

fn first_object_param_error(
    fields: &BTreeMap<String, VmValue>,
    schema: &BTreeMap<String, VmValue>,
    root_schema: &BTreeMap<String, VmValue>,
    param_name: &str,
    options: ValidationOptions,
) -> Option<String> {
    let mut known_keys = std::collections::BTreeSet::new();

    if let Some(VmValue::List(required_keys)) = schema.get("required") {
        for key_value in required_keys.iter() {
            let key = key_value.display();
            if !fields.contains_key(&key) {
                let expected = schema
                    .get("properties")
                    .and_then(VmValue::as_dict)
                    .and_then(|props| props.get(&key))
                    .and_then(VmValue::as_dict)
                    .map(schema_expected_label)
                    .unwrap_or_else(|| "value".to_string());
                return Some(format!(
                    "parameter '{}': missing field '{}' ({})",
                    param_name, key, expected
                ));
            }
        }
    }

    if let Some(VmValue::Dict(prop_schemas)) = schema.get("properties") {
        for (key, prop_schema) in prop_schemas.iter() {
            known_keys.insert(key.clone());
            let Some(prop_value) = fields.get(key) else {
                continue;
            };
            let Some(prop_schema_dict) = prop_schema.as_dict() else {
                continue;
            };

            if schema_is_object_like(prop_schema_dict) {
                match prop_value {
                    VmValue::Dict(_) | VmValue::StructInstance { .. } => {
                        let child_param = format!("{param_name}.{key}");
                        if let Some(error) = first_param_validation_error(
                            prop_value,
                            prop_schema_dict,
                            root_schema,
                            &child_param,
                            options,
                        ) {
                            return Some(error);
                        }
                    }
                    _ => {
                        return Some(format!(
                            "parameter '{}': field '{}' expected dict or struct, got {}",
                            param_name,
                            key,
                            prop_value.type_name()
                        ));
                    }
                }
                continue;
            }

            if let Some(expected_type) = schema_type_name(prop_schema_dict) {
                if !value_matches_type(prop_value, expected_type, options.numeric_compat) {
                    return Some(format!(
                        "parameter '{}': field '{}' expected {}, got {}",
                        param_name,
                        key,
                        expected_type,
                        actual_value_type(prop_value)
                    ));
                }
            }

            let child_param = format!("{param_name}.{key}");
            if let Some(error) = first_param_validation_error(
                prop_value,
                prop_schema_dict,
                root_schema,
                &child_param,
                options,
            ) {
                return Some(error);
            }
        }
    }

    match schema.get("additional_properties") {
        Some(VmValue::Bool(false)) => {
            for key in fields.keys() {
                if !known_keys.contains(key) {
                    return Some(format!(
                        "parameter '{}': unexpected field '{}'",
                        param_name, key
                    ));
                }
            }
        }
        Some(VmValue::Dict(extra_schema)) => {
            for (key, value) in fields.iter() {
                if known_keys.contains(key) {
                    continue;
                }
                if schema_is_object_like(extra_schema) {
                    match value {
                        VmValue::Dict(_) | VmValue::StructInstance { .. } => {
                            let child_param = format!("{param_name}.{key}");
                            if let Some(error) = first_param_validation_error(
                                value,
                                extra_schema,
                                root_schema,
                                &child_param,
                                options,
                            ) {
                                return Some(error);
                            }
                        }
                        _ => {
                            return Some(format!(
                                "parameter '{}': field '{}' expected dict or struct, got {}",
                                param_name,
                                key,
                                value.type_name()
                            ));
                        }
                    }
                    continue;
                }

                if let Some(expected_type) = schema_type_name(extra_schema) {
                    if !value_matches_type(value, expected_type, options.numeric_compat) {
                        return Some(format!(
                            "parameter '{}': field '{}' expected {}, got {}",
                            param_name,
                            key,
                            expected_type,
                            actual_value_type(value)
                        ));
                    }
                }

                let child_param = format!("{param_name}.{key}");
                if let Some(error) = first_param_validation_error(
                    value,
                    extra_schema,
                    root_schema,
                    &child_param,
                    options,
                ) {
                    return Some(error);
                }
            }
        }
        _ => {}
    }

    None
}

fn schema_is_object_like(schema: &BTreeMap<String, VmValue>) -> bool {
    schema_type_name(schema) == Some("dict")
        || schema.contains_key("properties")
        || schema.contains_key("required")
        || schema.contains_key("additional_properties")
}

fn schema_expected_label(schema: &BTreeMap<String, VmValue>) -> String {
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

fn schema_type_name(schema: &BTreeMap<String, VmValue>) -> Option<&str> {
    match schema.get("type") {
        Some(VmValue::String(type_name)) => Some(type_name.as_ref()),
        _ => None,
    }
}

fn value_matches_type(value: &VmValue, expected: &str, numeric_compat: bool) -> bool {
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

fn actual_value_type(value: &VmValue) -> &'static str {
    match value {
        VmValue::StructInstance { .. } => "dict",
        _ => value.type_name(),
    }
}

fn resolve_canonical_ref(
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

fn canonicalize_schema_value(schema: &VmValue) -> Result<VmValue, String> {
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

    // Preserve common metadata fields as-is.
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

fn canonical_to_json_schema(schema: &VmValue, openapi_style: bool) -> serde_json::Value {
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

fn vm_value_to_serde_json(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(value) => serde_json::Value::Bool(*value),
        VmValue::Int(value) => serde_json::json!(value),
        VmValue::Float(value) => serde_json::json!(value),
        VmValue::String(value) => serde_json::Value::String(value.to_string()),
        VmValue::List(items) | VmValue::Set(items) => {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> VmValue {
        VmValue::String(Rc::from(v))
    }

    fn make_dict(pairs: Vec<(&str, VmValue)>) -> BTreeMap<String, VmValue> {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    fn make_vm_dict(pairs: Vec<(&str, VmValue)>) -> VmValue {
        VmValue::Dict(Rc::new(make_dict(pairs)))
    }

    fn make_list(items: Vec<VmValue>) -> VmValue {
        VmValue::List(Rc::new(items))
    }

    #[test]
    fn normalize_json_schema_types() {
        let schema = make_vm_dict(vec![
            ("type", s("object")),
            (
                "properties",
                make_vm_dict(vec![("name", make_vm_dict(vec![("type", s("string"))]))]),
            ),
        ]);
        let normalized = canonicalize_schema_value(&schema).unwrap();
        let dict = normalized.as_dict().unwrap();
        assert_eq!(dict.get("type").unwrap().display(), "dict");
        let props = dict.get("properties").unwrap().as_dict().unwrap();
        assert_eq!(
            props
                .get("name")
                .unwrap()
                .as_dict()
                .unwrap()
                .get("type")
                .unwrap()
                .display(),
            "string"
        );
    }

    #[test]
    fn validate_additional_properties_false() {
        let schema = make_vm_dict(vec![
            ("type", s("dict")),
            ("additional_properties", VmValue::Bool(false)),
            (
                "properties",
                make_vm_dict(vec![("name", make_vm_dict(vec![("type", s("string"))]))]),
            ),
        ]);
        let result = schema_result_value(
            &make_vm_dict(vec![("name", s("Ada")), ("extra", s("x"))]),
            &schema,
            false,
        );
        assert!(matches!(
            result,
            VmValue::EnumVariant { variant, .. } if variant == "Err"
        ));
    }

    #[test]
    fn validate_union_type_array_input() {
        let schema = make_vm_dict(vec![("type", make_list(vec![s("string"), s("integer")]))]);
        assert!(schema_is_value(&VmValue::Int(4), &schema).unwrap());
        assert!(schema_is_value(&s("ok"), &schema).unwrap());
        assert!(!schema_is_value(&VmValue::Bool(true), &schema).unwrap());
    }

    #[test]
    fn export_openapi_nullable() {
        let schema = make_vm_dict(vec![
            ("type", s("string")),
            ("nullable", VmValue::Bool(true)),
        ]);
        let exported = schema_to_openapi_schema_value(&schema).unwrap();
        let dict = exported.as_dict().unwrap();
        assert_eq!(dict.get("type").unwrap().display(), "string");
        assert_eq!(dict.get("nullable").unwrap().display(), "true");
    }

    #[test]
    fn schema_partial_removes_required_recursively() {
        let schema = make_dict(vec![
            ("type", s("dict")),
            ("required", make_list(vec![s("nested")])),
            (
                "properties",
                make_vm_dict(vec![(
                    "nested",
                    make_vm_dict(vec![
                        ("type", s("dict")),
                        ("required", make_list(vec![s("x")])),
                        (
                            "properties",
                            make_vm_dict(vec![("x", make_vm_dict(vec![("type", s("int"))]))]),
                        ),
                    ]),
                )]),
            ),
        ]);
        let partial = schema_partial_dict(&schema);
        assert!(!partial.contains_key("required"));
        let nested = partial
            .get("properties")
            .unwrap()
            .as_dict()
            .unwrap()
            .get("nested")
            .unwrap()
            .as_dict()
            .unwrap();
        assert!(nested.get("required").is_none());
    }

    #[test]
    fn merge_schema_dicts_basic() {
        let base = make_dict(vec![("type", s("dict")), ("title", s("Base"))]);
        let overrides = make_dict(vec![("title", s("Override")), ("extra", s("yes"))]);
        let merged = merge_schema_dicts(&base, &overrides);
        assert_eq!(merged.get("type").unwrap().display(), "dict");
        assert_eq!(merged.get("title").unwrap().display(), "Override");
        assert_eq!(merged.get("extra").unwrap().display(), "yes");
    }
}
