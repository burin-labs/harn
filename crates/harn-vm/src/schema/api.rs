use std::rc::Rc;

use crate::value::{VmError, VmValue};

use super::canonicalize::{canonical_to_json_schema, canonicalize_schema_value};
use super::result::{result_err_value, result_ok_value};
use super::transform::{
    merge_schema_dicts, schema_omit_dict, schema_partial_dict, schema_pick_dict,
};
use super::type_check::schema_is_object_like;
use super::validate::{first_param_validation_error, validate_schema_value, ValidationOptions};

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
    Ok(super::json_to_vm_value(&canonical_to_json_schema(
        &normalized,
        false,
    )))
}

pub(crate) fn schema_to_openapi_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    let normalized = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(Rc::from(error))))?;
    Ok(super::json_to_vm_value(&canonical_to_json_schema(
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
