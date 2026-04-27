use std::collections::BTreeMap;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use crate::value::{values_equal, StructLayout, VmValue};

use super::canonicalize::resolve_canonical_ref;
use super::result::ValidationResult;
use super::type_check::{
    actual_value_type, schema_expected_label, schema_is_object_like, schema_type_name,
    value_matches_type,
};
use super::{child_path, index_path, location_label, schema_bool, schema_i64, schema_number};

// Schema patterns are typically a small, recurring set (e.g. `"^[a-z]+$"`),
// and recompiling them on every value validated showed up as a hot-path
// allocation cost. Cap the cache so adversarial schemas can't grow memory
// unboundedly.
const PATTERN_CACHE_LIMIT: usize = 256;

#[derive(Clone)]
enum PatternEntry {
    Compiled(Arc<regex::Regex>),
    Invalid(Arc<String>),
}

fn pattern_cache() -> &'static Mutex<HashMap<String, PatternEntry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, PatternEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_pattern(pattern: &str) -> PatternEntry {
    if let Ok(mut cache) = pattern_cache().lock() {
        if let Some(entry) = cache.get(pattern) {
            return entry.clone();
        }
        let entry = match regex::Regex::new(pattern) {
            Ok(re) => PatternEntry::Compiled(Arc::new(re)),
            Err(error) => PatternEntry::Invalid(Arc::new(error.to_string())),
        };
        if cache.len() >= PATTERN_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(pattern.to_string(), entry.clone());
        return entry;
    }
    match regex::Regex::new(pattern) {
        Ok(re) => PatternEntry::Compiled(Arc::new(re)),
        Err(error) => PatternEntry::Invalid(Arc::new(error.to_string())),
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ValidationOptions {
    pub(super) apply_defaults: bool,
    pub(super) numeric_compat: bool,
}

pub(super) fn validate_schema_value(
    data: &VmValue,
    schema: &VmValue,
    options: ValidationOptions,
) -> ValidationResult {
    let root = schema.as_dict().cloned().unwrap_or_default();
    let schema_dict = schema.as_dict().cloned().unwrap_or_default();
    validate_against_schema(data, &schema_dict, &root, "", options)
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
        VmValue::StructInstance { layout, .. } => {
            let fields = value.struct_fields_map().unwrap_or_default();
            let (next_value, next_errors) =
                validate_object_fields(&fields, Some(layout), schema, root_schema, path, options);
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
                match cached_pattern(pattern) {
                    PatternEntry::Compiled(re) => {
                        if !re.is_match(text) {
                            errors.push(format!(
                                "at {}: value does not match pattern '{}'",
                                location_label(path),
                                pattern
                            ));
                        }
                    }
                    PatternEntry::Invalid(error) => errors.push(format!(
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
    struct_layout: Option<&StructLayout>,
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

    let normalized = if let Some(layout) = struct_layout {
        let mut field_names = layout.field_names().to_vec();
        for key in merged.keys() {
            if layout.field_index(key).is_none() {
                field_names.push(key.clone());
            }
        }
        VmValue::struct_instance_with_layout(layout.struct_name().to_string(), field_names, merged)
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

pub(super) fn first_param_validation_error(
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
        let struct_fields;
        let fields = match value {
            VmValue::Dict(map) => map.as_ref(),
            VmValue::StructInstance { .. } => {
                struct_fields = value.struct_fields_map().unwrap_or_default();
                &struct_fields
            }
            _ => {
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
                let key_initial = key.chars().next();
                let suggestion = crate::value::closest_match(
                    &key,
                    fields
                        .keys()
                        .map(String::as_str)
                        .filter(|candidate| candidate.chars().next() == key_initial),
                );
                let expected = schema
                    .get("properties")
                    .and_then(VmValue::as_dict)
                    .and_then(|props| props.get(&key))
                    .and_then(VmValue::as_dict)
                    .map(schema_expected_label)
                    .unwrap_or_else(|| "value".to_string());
                if let Some(suggestion) = suggestion {
                    return Some(format!(
                        "parameter '{}': missing field '{}' ({}), did you mean '{}'?",
                        param_name, key, expected, suggestion
                    ));
                }
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
