use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::runtime_limits::RuntimeLimits;
use crate::value::value_structural_hash_key;
use crate::value::{VmError, VmValue};

use super::canonicalize::{canonical_to_json_schema, canonicalize_schema_value};
use super::result::{result_err_value, result_ok_value};
use super::transform::{
    merge_schema_dicts, schema_omit_dict, schema_partial_dict, schema_pick_dict,
};
use super::type_check::schema_is_object_like;
use super::validate::{first_param_validation_error, validate_schema_value, ValidationOptions};

const PARAM_SCHEMA_CACHE_LIMIT: usize = RuntimeLimits::DEFAULT.max_schema_guard_cache_entries;

thread_local! {
    static PARAM_SCHEMA_CACHE: RefCell<HashMap<String, Result<CanonicalParamSchema, Rc<str>>>> =
        RefCell::new(HashMap::new());
}

#[derive(Debug, Clone)]
pub(crate) struct CanonicalParamSchema {
    schema: Arc<crate::value::DictMap>,
    object_like: bool,
}

pub(crate) fn canonical_param_schema(schema: &VmValue) -> Result<CanonicalParamSchema, String> {
    let normalized = canonicalize_schema_value(schema)?;
    let VmValue::Dict(schema) = normalized else {
        return Err("schema must be a dict".to_string());
    };
    let object_like = schema_is_object_like(&schema);
    Ok(CanonicalParamSchema {
        schema,
        object_like,
    })
}

fn cached_canonical_param_schema(schema: &VmValue) -> Result<CanonicalParamSchema, String> {
    let key = value_structural_hash_key(schema);
    PARAM_SCHEMA_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(cached) = cache.get(&key) {
            return cached.clone().map_err(|error| error.as_ref().to_string());
        }

        let canonical = canonical_param_schema(schema).map_err(Rc::<str>::from);
        if cache.len() >= PARAM_SCHEMA_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, canonical.clone());
        canonical.map_err(|error| error.as_ref().to_string())
    })
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
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))?;
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
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))?;
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
        Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            result.errors.join("; "),
        ))))
    }
}

pub(crate) fn schema_assert_param(
    value: &VmValue,
    param_name: &str,
    schema: &VmValue,
) -> Result<(), VmError> {
    let normalized = cached_canonical_param_schema(schema)
        .map_err(|error| VmError::TypeError(format!("parameter '{param_name}': {error}")))?;
    schema_assert_canonical_param(value, param_name, &normalized)
}

pub(crate) fn schema_assert_canonical_param(
    value: &VmValue,
    param_name: &str,
    schema: &CanonicalParamSchema,
) -> Result<(), VmError> {
    let options = ValidationOptions {
        apply_defaults: false,
        numeric_compat: true,
    };
    let schema_dict = schema.schema.as_ref();
    if let Some(error) =
        first_param_validation_error(value, schema_dict, schema_dict, param_name, options)
    {
        return if schema.object_like {
            Err(VmError::TypeError(error))
        } else {
            Err(VmError::Runtime(format!("TypeError: {error}")))
        };
    }
    Ok(())
}

pub(crate) fn schema_to_json_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    let normalized = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))?;
    let json_schema = canonical_to_json_schema(&normalized, false)
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))?;
    Ok(super::json_to_vm_value(&json_schema))
}

pub(crate) fn schema_to_openapi_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    let normalized = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))?;
    let json_schema = canonical_to_json_schema(&normalized, true)
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))?;
    Ok(super::json_to_vm_value(&json_schema))
}

pub(crate) fn schema_from_json_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))
}

pub(crate) fn schema_from_openapi_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))
}

pub(crate) fn schema_extend_value(base: &VmValue, overrides: &VmValue) -> Result<VmValue, VmError> {
    let base = canonicalize_schema_value(base)
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))?;
    let overrides = canonicalize_schema_value(overrides)
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))?;
    let base_dict = base.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "schema_extend: schema must be a dict",
        )))
    })?;
    let overrides_dict = overrides.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "schema_extend: schema must be a dict",
        )))
    })?;
    Ok(VmValue::dict(merge_schema_dicts(base_dict, overrides_dict)))
}

pub(crate) fn schema_partial_value(schema: &VmValue) -> Result<VmValue, VmError> {
    let schema = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))?;
    let schema_dict = schema.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "schema_partial: schema must be a dict",
        )))
    })?;
    Ok(VmValue::dict(schema_partial_dict(schema_dict)))
}

pub(crate) fn schema_pick_value(schema: &VmValue, keys: &[String]) -> Result<VmValue, VmError> {
    let schema = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))?;
    let schema_dict = schema.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "schema_pick: schema must be a dict",
        )))
    })?;
    Ok(VmValue::dict(schema_pick_dict(schema_dict, keys)))
}

pub(crate) fn schema_omit_value(schema: &VmValue, keys: &[String]) -> Result<VmValue, VmError> {
    let schema = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::String(arcstr::ArcStr::from(error))))?;
    let schema_dict = schema.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "schema_omit: schema must be a dict",
        )))
    })?;
    Ok(VmValue::dict(schema_omit_dict(schema_dict, keys)))
}
