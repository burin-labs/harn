use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use crate::value::{StructLayout, VmValue};

use super::canonicalize::{canonical_to_json_schema, resolve_canonical_ref_with_path};
use super::limits::SchemaTraversal;
use super::result::{ValidationIssue, ValidationResult};
use super::type_check::{actual_value_type, schema_type_name, value_matches_type};
use super::validate::ValidationOptions;
use super::{child_path, index_path, vm_value_to_serde_json};

const MAX_CACHED_SCHEMA_BYTES: usize = 64 * 1024;

#[derive(Clone)]
enum CompiledValidator {
    Ready(Arc<jsonschema::Validator>),
    Invalid(Arc<str>),
}

fn validator_cache() -> &'static quick_cache::sync::Cache<String, CompiledValidator> {
    static CACHE: OnceLock<quick_cache::sync::Cache<String, CompiledValidator>> = OnceLock::new();
    CACHE.get_or_init(|| {
        quick_cache::sync::Cache::new(
            crate::runtime_limits::RuntimeLimits::DEFAULT.max_schema_validator_cache_entries,
        )
    })
}

fn compile_validator(schema: &serde_json::Value) -> CompiledValidator {
    if !schema_is_cacheable(schema) {
        return build_validator(schema);
    }
    let serialized = serde_json::to_vec(schema).expect("JSON values always serialize");
    if serialized.len() > MAX_CACHED_SCHEMA_BYTES {
        return build_validator(schema);
    }
    let cache_key = blake3::hash(&serialized).to_hex().to_string();
    if let Some(validator) = validator_cache().get(&cache_key) {
        return validator;
    }
    let validator = build_validator(schema);
    validator_cache().insert(cache_key, validator.clone());
    validator
}

fn build_validator(schema: &serde_json::Value) -> CompiledValidator {
    match jsonschema::draft202012::new(schema) {
        Ok(validator) => CompiledValidator::Ready(Arc::new(validator)),
        Err(error) => CompiledValidator::Invalid(Arc::from(error.to_string())),
    }
}

fn schema_is_cacheable(schema: &serde_json::Value) -> bool {
    // A compiled validator owns literal constraint and annotation values. Do
    // not extend the lifetime of caller data that could contain a secret.
    let mut pending = vec![schema];
    while let Some(value) = pending.pop() {
        match value {
            serde_json::Value::Object(object) => {
                if object.keys().any(|key| {
                    matches!(
                        key.as_str(),
                        "const" | "enum" | "default" | "example" | "examples"
                    )
                }) {
                    return false;
                }
                pending.extend(object.values());
            }
            serde_json::Value::Array(items) => pending.extend(items),
            _ => {}
        }
    }
    true
}

pub(super) fn validate_schema_value(
    data: &VmValue,
    schema: &VmValue,
    options: ValidationOptions,
) -> ValidationResult {
    let root = schema.as_dict().cloned().unwrap_or_default();
    let mut traversal = SchemaTraversal::new();
    let normalized = if options.apply_defaults {
        apply_defaults(data, &root, &root, options.numeric_compat, &mut traversal)
    } else {
        data.clone()
    };

    let mut traversal = SchemaTraversal::new();
    let mut ref_stack = Vec::new();
    let mut errors = Vec::new();
    validate_harn_types(
        &normalized,
        &root,
        &root,
        "",
        options.numeric_compat,
        &mut traversal,
        &mut ref_stack,
        &mut errors,
    );
    if errors.is_empty() {
        errors.extend(validate_json_schema(
            &normalized,
            schema,
            options.numeric_compat,
        ));
    }
    ValidationResult {
        value: normalized,
        errors,
    }
}

pub(super) fn validate_schema_fragment(
    value: &VmValue,
    schema: &crate::value::DictMap,
    root: &crate::value::DictMap,
    path: &str,
    options: ValidationOptions,
) -> ValidationResult {
    let mut traversal = SchemaTraversal::new();
    let mut ref_stack = Vec::new();
    let mut errors = Vec::new();
    validate_harn_types(
        value,
        schema,
        root,
        path,
        options.numeric_compat,
        &mut traversal,
        &mut ref_stack,
        &mut errors,
    );
    if errors.is_empty() {
        let fragment = VmValue::dict(schema.clone());
        let root = VmValue::dict(root.clone());
        errors.extend(validate_json_schema_fragment(
            value,
            &fragment,
            &root,
            options.numeric_compat,
            path,
        ));
    }
    ValidationResult {
        value: value.clone(),
        errors,
    }
}

fn validate_json_schema_fragment(
    value: &VmValue,
    schema: &VmValue,
    root: &VmValue,
    numeric_compat: bool,
    path_prefix: &str,
) -> Vec<ValidationIssue> {
    let mut json_schema = match canonical_to_json_schema(schema, false) {
        Ok(schema) => schema,
        Err(error) => return vec![ValidationIssue::schema(path_prefix, error)],
    };
    if let (Some(fragment), Ok(serde_json::Value::Object(root))) = (
        json_schema.as_object_mut(),
        canonical_to_json_schema(root, false),
    ) {
        for key in ["definitions", "$defs", "components"] {
            if let Some(value) = root.get(key) {
                fragment
                    .entry(key.to_string())
                    .or_insert_with(|| value.clone());
            }
        }
    }
    validate_exported_json_schema(value, json_schema, numeric_compat, path_prefix)
}

fn validate_json_schema(
    value: &VmValue,
    schema: &VmValue,
    numeric_compat: bool,
) -> Vec<ValidationIssue> {
    let json_schema = match canonical_to_json_schema(schema, false) {
        Ok(schema) => schema,
        Err(error) => return vec![ValidationIssue::schema("", error)],
    };
    validate_exported_json_schema(value, json_schema, numeric_compat, "")
}

fn validate_exported_json_schema(
    value: &VmValue,
    mut json_schema: serde_json::Value,
    numeric_compat: bool,
    path_prefix: &str,
) -> Vec<ValidationIssue> {
    sanitize_schema_types(&mut json_schema, numeric_compat);
    let validator = match compile_validator(&json_schema) {
        CompiledValidator::Ready(validator) => validator,
        CompiledValidator::Invalid(error) => {
            return vec![ValidationIssue::schema(
                path_prefix,
                format!("invalid JSON Schema: {error}"),
            )]
        }
    };
    let instance = vm_value_to_serde_json(value);
    validator
        .iter_errors(&instance)
        .map(|error| {
            let suffix = json_pointer_to_harn_path(&error.instance_path().to_string(), &instance);
            let path = if suffix.is_empty() {
                path_prefix.to_string()
            } else if suffix.starts_with('[') && !path_prefix.is_empty() {
                format!("{path_prefix}{suffix}")
            } else {
                child_path(path_prefix, &suffix)
            };
            ValidationIssue::schema(&path, error.to_string())
        })
        .collect()
}

fn sanitize_schema_types(schema: &mut serde_json::Value, numeric_compat: bool) {
    match schema {
        serde_json::Value::Object(object) => {
            if let Some(serde_json::Value::String(kind)) = object.get_mut("type") {
                if numeric_compat && kind == "integer" {
                    *kind = "number".to_string();
                } else if !matches!(
                    kind.as_str(),
                    "array" | "boolean" | "integer" | "null" | "number" | "object" | "string"
                ) {
                    object.remove("type");
                }
            }
            for child in object.values_mut() {
                sanitize_schema_types(child, numeric_compat);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                sanitize_schema_types(item, numeric_compat);
            }
        }
        _ => {}
    }
}

fn json_pointer_to_harn_path(pointer: &str, instance: &serde_json::Value) -> String {
    let mut path = String::new();
    let mut current = Some(instance);
    for raw in pointer.split('/').skip(1) {
        let segment = raw.replace("~1", "/").replace("~0", "~");
        if let Some(serde_json::Value::Array(items)) = current {
            path.push('[');
            path.push_str(&segment);
            path.push(']');
            current = segment
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get(index));
        } else if path.is_empty() {
            path.push_str(&segment);
            current = current
                .and_then(serde_json::Value::as_object)
                .and_then(|object| object.get(&segment));
        } else {
            path.push('.');
            path.push_str(&segment);
            current = current
                .and_then(serde_json::Value::as_object)
                .and_then(|object| object.get(&segment));
        }
    }
    path
}

#[allow(clippy::too_many_arguments)]
fn validate_harn_types(
    value: &VmValue,
    schema: &crate::value::DictMap,
    root: &crate::value::DictMap,
    path: &str,
    numeric_compat: bool,
    traversal: &mut SchemaTraversal,
    ref_stack: &mut Vec<String>,
    errors: &mut Vec<ValidationIssue>,
) {
    if let Err(error) = traversal.enter_schema() {
        errors.push(ValidationIssue::schema(path, error));
        return;
    }
    validate_harn_types_inner(
        value,
        schema,
        root,
        path,
        numeric_compat,
        traversal,
        ref_stack,
        errors,
    );
    traversal.exit_schema();
}

#[allow(clippy::too_many_arguments)]
fn validate_harn_types_inner(
    value: &VmValue,
    schema: &crate::value::DictMap,
    root: &crate::value::DictMap,
    path: &str,
    numeric_compat: bool,
    traversal: &mut SchemaTraversal,
    ref_stack: &mut Vec<String>,
    errors: &mut Vec<ValidationIssue>,
) {
    if let Some(VmValue::String(pointer)) = schema.get("$ref") {
        if let Err(error) = traversal.expand_ref() {
            errors.push(ValidationIssue::schema(path, error));
            return;
        }
        let Some((resolved_pointer, resolved)) = resolve_canonical_ref_with_path(root, pointer)
        else {
            errors.push(ValidationIssue::schema(
                path,
                format!("unresolved schema reference '{pointer}'"),
            ));
            return;
        };
        if let Some(index) = ref_stack
            .iter()
            .position(|entry| entry == &resolved_pointer)
        {
            let mut cycle = ref_stack[index..].to_vec();
            cycle.push(resolved_pointer);
            errors.push(ValidationIssue::schema(
                path,
                format!("cyclic schema reference: {}", cycle.join(" -> ")),
            ));
            return;
        }
        ref_stack.push(resolved_pointer);
        validate_harn_types(
            value,
            &resolved,
            root,
            path,
            numeric_compat,
            traversal,
            ref_stack,
            errors,
        );
        ref_stack.pop();
        return;
    }
    if matches!(value, VmValue::Nil) && super::schema_bool(schema, "nullable") {
        return;
    }
    if let Some(VmValue::List(branches)) = schema.get("union") {
        let matched = branches.iter().filter_map(VmValue::as_dict).any(|branch| {
            let mut branch_errors = Vec::new();
            validate_harn_types(
                value,
                branch,
                root,
                path,
                numeric_compat,
                traversal,
                ref_stack,
                &mut branch_errors,
            );
            branch_errors.is_empty()
        });
        if !matched {
            errors.push(ValidationIssue::schema(
                path,
                "value did not match any union branch",
            ));
        }
        return;
    }
    if let Some(VmValue::List(branches)) = schema.get("all_of") {
        for branch in branches.iter().filter_map(VmValue::as_dict) {
            validate_harn_types(
                value,
                branch,
                root,
                path,
                numeric_compat,
                traversal,
                ref_stack,
                errors,
            );
        }
    }
    if let Some(expected) = schema_type_name(schema) {
        if !value_matches_type(value, expected, numeric_compat) {
            errors.push(ValidationIssue::schema(
                path,
                format!(
                    "expected type '{}', got '{}'",
                    expected,
                    actual_value_type(value)
                ),
            ));
            return;
        }
    }

    let fields = match value {
        VmValue::Dict(fields) => Some(fields.as_ref()),
        VmValue::StructInstance(_) => None,
        _ => None,
    };
    let struct_fields = value.struct_fields_map();
    if let Some(fields) = fields.or(struct_fields.as_ref()) {
        let mut known = BTreeSet::new();
        if let Some(VmValue::Dict(properties)) = schema.get("properties") {
            for (name, child_schema) in properties.iter() {
                known.insert(name.clone());
                if let (Some(child), Some(child_schema)) =
                    (fields.get(name), child_schema.as_dict())
                {
                    validate_harn_types(
                        child,
                        child_schema,
                        root,
                        &child_path(path, name),
                        numeric_compat,
                        traversal,
                        ref_stack,
                        errors,
                    );
                }
            }
        }
        if let Some(VmValue::Dict(extra_schema)) = schema.get("additional_properties") {
            for (name, child) in fields.iter().filter(|(name, _)| !known.contains(*name)) {
                validate_harn_types(
                    child,
                    extra_schema,
                    root,
                    &child_path(path, name),
                    numeric_compat,
                    traversal,
                    ref_stack,
                    errors,
                );
            }
        }
    } else if let (Some(items), Some(VmValue::Dict(item_schema))) =
        (collection_items(value), schema.get("items"))
    {
        for (index, child) in items.iter().enumerate() {
            validate_harn_types(
                child,
                item_schema,
                root,
                &index_path(path, index),
                numeric_compat,
                traversal,
                ref_stack,
                errors,
            );
        }
    }
}

fn collection_items(value: &VmValue) -> Option<&[VmValue]> {
    match value {
        VmValue::List(items) => Some(items),
        VmValue::Set(items) => Some(items.items()),
        _ => None,
    }
}

fn apply_defaults(
    value: &VmValue,
    schema: &crate::value::DictMap,
    root: &crate::value::DictMap,
    numeric_compat: bool,
    traversal: &mut SchemaTraversal,
) -> VmValue {
    if traversal.enter_schema().is_err() {
        return value.clone();
    }
    let normalized = apply_defaults_inner(value, schema, root, numeric_compat, traversal);
    traversal.exit_schema();
    normalized
}

fn apply_defaults_inner(
    value: &VmValue,
    schema: &crate::value::DictMap,
    root: &crate::value::DictMap,
    numeric_compat: bool,
    traversal: &mut SchemaTraversal,
) -> VmValue {
    if let Some(VmValue::String(pointer)) = schema.get("$ref") {
        if traversal.expand_ref().is_err() {
            return value.clone();
        }
        return resolve_canonical_ref_with_path(root, pointer).map_or_else(
            || value.clone(),
            |(_, resolved)| apply_defaults(value, &resolved, root, numeric_compat, traversal),
        );
    }
    let mut normalized = value.clone();
    if let Some(VmValue::List(branches)) = schema.get("all_of") {
        for branch in branches.iter().filter_map(VmValue::as_dict) {
            normalized = apply_defaults(&normalized, branch, root, numeric_compat, traversal);
        }
    }
    if let Some(VmValue::List(branches)) = schema.get("union") {
        for branch in branches.iter().filter_map(VmValue::as_dict) {
            let candidate = apply_defaults(&normalized, branch, root, numeric_compat, traversal);
            let result = validate_schema_fragment(
                &candidate,
                branch,
                root,
                "",
                ValidationOptions {
                    apply_defaults: false,
                    numeric_compat,
                },
            );
            if result.errors.is_empty() {
                normalized = candidate;
                break;
            }
        }
    }

    let original_layout = match &normalized {
        VmValue::StructInstance(instance) => Some(instance.layout.clone()),
        _ => None,
    };
    if let Some(mut fields) = normalized
        .struct_fields_map()
        .or_else(|| normalized.as_dict().map(|fields| fields.as_ref().clone()))
    {
        if let Some(VmValue::Dict(properties)) = schema.get("properties") {
            for (name, child_schema) in properties.iter() {
                let Some(child_schema) = child_schema.as_dict() else {
                    continue;
                };
                if let Some(child) = fields.get(name).cloned() {
                    fields.insert(
                        name.clone(),
                        apply_defaults(&child, child_schema, root, numeric_compat, traversal),
                    );
                } else if let Some(default) = child_schema.get("default") {
                    fields.insert(
                        name.clone(),
                        apply_defaults(default, child_schema, root, numeric_compat, traversal),
                    );
                }
            }
        }
        return match original_layout {
            Some(layout) => struct_with_fields(&layout, fields),
            None => VmValue::dict(fields),
        };
    }
    if let (Some(items), Some(VmValue::Dict(item_schema))) =
        (collection_items(&normalized), schema.get("items"))
    {
        let values = items
            .iter()
            .map(|item| apply_defaults(item, item_schema, root, numeric_compat, traversal))
            .collect::<Vec<_>>();
        return if matches!(normalized, VmValue::Set(_)) {
            VmValue::set(values)
        } else {
            VmValue::List(std::sync::Arc::new(values))
        };
    }
    normalized
}

fn struct_with_fields(layout: &StructLayout, fields: crate::value::DictMap) -> VmValue {
    let mut field_names = layout.field_names().to_vec();
    for key in fields.keys() {
        if layout.field_index(key).is_none() {
            field_names.push(key.to_string());
        }
    }
    VmValue::struct_instance_with_layout(layout.struct_name().to_string(), field_names, fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiled_validator_cache_reuses_validators() {
        let schema = serde_json::json!({"type": "string", "minLength": 2});
        let CompiledValidator::Ready(first) = compile_validator(&schema) else {
            panic!("valid schema did not compile");
        };
        let CompiledValidator::Ready(second) = compile_validator(&schema) else {
            panic!("cached schema did not compile");
        };
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn compiled_validator_cache_does_not_retain_literal_schema_data() {
        let schema = serde_json::json!({"const": "sensitive-value"});
        let CompiledValidator::Ready(first) = compile_validator(&schema) else {
            panic!("valid schema did not compile");
        };
        let CompiledValidator::Ready(second) = compile_validator(&schema) else {
            panic!("valid schema did not compile");
        };
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn json_pointer_paths_distinguish_array_indices_from_numeric_keys() {
        let instance = serde_json::json!({
            "items": [{"name": false}],
            "123": {"value": false}
        });
        assert_eq!(
            json_pointer_to_harn_path("/items/0/name", &instance),
            "items[0].name"
        );
        assert_eq!(
            json_pointer_to_harn_path("/123/value", &instance),
            "123.value"
        );
    }
}
