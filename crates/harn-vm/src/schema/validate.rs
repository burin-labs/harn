use crate::value::VmValue;

use super::canonicalize::resolve_canonical_ref_with_path;
use super::jsonschema_validate;
use super::limits::SchemaTraversal;
use super::result::ValidationResult;
use super::schema_bool;
use super::type_check::{
    actual_value_type, schema_expected_label, schema_is_object_like, schema_type_name,
    value_matches_type,
};

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
    jsonschema_validate::validate_schema_value(data, schema, options)
}

struct ParamValidationContext {
    traversal: SchemaTraversal,
    ref_stack: Vec<String>,
}

impl ParamValidationContext {
    fn new() -> Self {
        Self {
            traversal: SchemaTraversal::new(),
            ref_stack: Vec::new(),
        }
    }
}

pub(super) fn first_param_validation_error(
    value: &VmValue,
    schema: &crate::value::DictMap,
    root_schema: &crate::value::DictMap,
    param_name: &str,
    options: ValidationOptions,
) -> Option<String> {
    first_param_validation_error_inner(
        value,
        schema,
        root_schema,
        param_name,
        options,
        &mut ParamValidationContext::new(),
    )
}

fn first_param_validation_error_inner(
    value: &VmValue,
    schema: &crate::value::DictMap,
    root_schema: &crate::value::DictMap,
    param_name: &str,
    options: ValidationOptions,
    context: &mut ParamValidationContext,
) -> Option<String> {
    if let Err(error) = context.traversal.enter_schema() {
        return Some(format!("parameter '{param_name}': {error}"));
    }
    let result =
        first_param_validation_error_body(value, schema, root_schema, param_name, options, context);
    context.traversal.exit_schema();
    result
}

fn first_param_validation_error_body(
    value: &VmValue,
    schema: &crate::value::DictMap,
    root_schema: &crate::value::DictMap,
    param_name: &str,
    options: ValidationOptions,
    context: &mut ParamValidationContext,
) -> Option<String> {
    if let Some(VmValue::String(pointer)) = schema.get("$ref") {
        if let Err(error) = context.traversal.expand_ref() {
            return Some(format!("parameter '{param_name}': {error}"));
        }
        let Some((resolved_pointer, resolved)) =
            resolve_canonical_ref_with_path(root_schema, pointer)
        else {
            return Some(format!(
                "parameter '{param_name}': unresolved schema reference '{pointer}'"
            ));
        };
        if let Some(index) = context
            .ref_stack
            .iter()
            .position(|entry| entry == &resolved_pointer)
        {
            let mut cycle = context.ref_stack[index..].to_vec();
            cycle.push(resolved_pointer);
            return Some(format!(
                "parameter '{}': cyclic schema reference: {}",
                param_name,
                cycle.join(" -> ")
            ));
        }
        context.ref_stack.push(resolved_pointer);
        let result = first_param_validation_error_inner(
            value,
            &resolved,
            root_schema,
            param_name,
            options,
            context,
        );
        context.ref_stack.pop();
        return result;
    }

    if matches!(value, VmValue::Nil) && schema_bool(schema, "nullable") {
        return None;
    }
    if let Some(VmValue::List(branches)) = schema.get("all_of") {
        for branch in branches.iter().filter_map(VmValue::as_dict) {
            if let Some(error) = first_param_validation_error_inner(
                value,
                branch,
                root_schema,
                param_name,
                options,
                context,
            ) {
                return Some(error);
            }
        }
        return None;
    }
    if let Some(VmValue::List(branches)) = schema.get("union") {
        if branches.iter().filter_map(VmValue::as_dict).any(|branch| {
            first_param_validation_error_inner(
                value,
                branch,
                root_schema,
                param_name,
                options,
                context,
            )
            .is_none()
        }) {
            return None;
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
            VmValue::StructInstance(_) => {
                struct_fields = value.struct_fields_map().unwrap_or_default();
                &struct_fields
            }
            _ => {
                return Some(format!(
                    "parameter '{}': expected dict or struct, got {}",
                    param_name,
                    value.type_name()
                ))
            }
        };
        return first_object_param_error(fields, schema, root_schema, param_name, options, context);
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
    let result =
        jsonschema_validate::validate_schema_fragment(value, schema, root_schema, "root", options);
    if result.errors.is_empty() {
        None
    } else {
        let detail = result
            .errors
            .into_iter()
            .map(|error| {
                let rendered = error.render();
                rendered
                    .strip_prefix("at root: ")
                    .unwrap_or(&rendered)
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("; ");
        Some(format!("parameter '{param_name}': {detail}"))
    }
}

fn first_object_param_error(
    fields: &crate::value::DictMap,
    schema: &crate::value::DictMap,
    root_schema: &crate::value::DictMap,
    param_name: &str,
    options: ValidationOptions,
    context: &mut ParamValidationContext,
) -> Option<String> {
    let mut known_keys = std::collections::BTreeSet::new();
    if let Some(VmValue::List(required_keys)) = schema.get("required") {
        for key_value in required_keys.iter() {
            let key = key_value.display();
            if fields.contains_key(key.as_str()) {
                continue;
            }
            let key_initial = key.chars().next();
            let suggestion = crate::value::closest_match(
                &key,
                fields
                    .keys()
                    .map(|key| key.as_str())
                    .filter(|candidate| candidate.chars().next() == key_initial),
            );
            let expected = schema
                .get("properties")
                .and_then(VmValue::as_dict)
                .and_then(|properties| properties.get(key.as_str()))
                .and_then(VmValue::as_dict)
                .map(schema_expected_label)
                .unwrap_or_else(|| "value".to_string());
            let actual_keys = fields.keys().map(|key| key.as_str()).collect::<Vec<_>>();
            let actual_summary = crate::stdlib::shapes::format_available_fields(&actual_keys);
            return Some(match suggestion {
                Some(suggestion) => format!(
                    "parameter '{param_name}': missing field '{key}' ({expected}), did you mean '{suggestion}'? — {actual_summary}"
                ),
                None => format!(
                    "parameter '{param_name}': missing field '{key}' ({expected}) — {actual_summary}"
                ),
            });
        }
    }

    if let Some(VmValue::Dict(properties)) = schema.get("properties") {
        for (key, child_schema) in properties.iter() {
            known_keys.insert(key.clone());
            let (Some(child), Some(child_schema)) = (fields.get(key), child_schema.as_dict())
            else {
                continue;
            };
            let child_param = format!("{param_name}.{key}");
            if let Some(error) = first_param_validation_error_inner(
                child,
                child_schema,
                root_schema,
                &child_param,
                options,
                context,
            ) {
                return Some(error);
            }
        }
    }

    match schema.get("additional_properties") {
        Some(VmValue::Bool(false)) => {
            if let Some(key) = fields.keys().find(|key| !known_keys.contains(*key)) {
                return Some(format!(
                    "parameter '{param_name}': unexpected field '{key}'"
                ));
            }
        }
        Some(VmValue::Dict(extra_schema)) => {
            for (key, value) in fields.iter().filter(|(key, _)| !known_keys.contains(*key)) {
                let child_param = format!("{param_name}.{key}");
                if let Some(error) = first_param_validation_error_inner(
                    value,
                    extra_schema,
                    root_schema,
                    &child_param,
                    options,
                    context,
                ) {
                    return Some(error);
                }
            }
        }
        _ => {}
    }
    None
}
