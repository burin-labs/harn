use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use crate::runtime_limits::RuntimeLimits;
use crate::value::value_structural_hash_key;
use crate::value::{VmDictExt, VmError, VmValue};

use super::canonicalize::{canonical_to_json_schema, canonicalize_schema_value};
use super::jsonschema_validate::{validate_schema_value_as, InputRepresentation};
use super::result::{
    issue_messages, issue_values, result_err_value, result_ok_value, ValidationIssue,
};
use super::transform::{
    merge_schema_dicts, schema_omit_dict, schema_partial_dict, schema_pick_dict,
};
use super::validate::{first_param_validation_error, validate_schema_value, ValidationOptions};

const PARAM_SCHEMA_CACHE_LIMIT: usize = RuntimeLimits::DEFAULT.max_schema_guard_cache_entries;

thread_local! {
    static PARAM_SCHEMA_CACHE: RefCell<HashMap<String, Result<CanonicalParamSchema, Rc<str>>>> =
        RefCell::new(HashMap::new());
}

#[derive(Debug, Clone)]
pub struct CanonicalParamSchema {
    schema: Arc<crate::value::DictMap>,
}

/// The first machine-readable schema failure projected for incremental
/// consumers. The validator owns all three fields so callers cannot pair an
/// exact kind and detail with a reconstructed or fallback path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaValidationIssue {
    pub kind: crate::value::SchemaValidationReasonKind,
    pub detail: String,
    pub path: String,
}

pub fn canonical_param_schema(schema: &VmValue) -> Result<CanonicalParamSchema, String> {
    let normalized = canonicalize_schema_value(schema)?;
    let VmValue::Dict(schema) = normalized else {
        return Err("schema must be a dict".to_string());
    };
    Ok(CanonicalParamSchema { schema })
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

pub fn schema_result_value(data: &VmValue, schema: &VmValue, apply_defaults: bool) -> VmValue {
    schema_result_value_as(data, schema, apply_defaults, InputRepresentation::Native)
}

/// Restore byte envelopes at their declared schema positions before validating JSON data.
pub fn schema_decode_json_value(data: &VmValue, schema: &VmValue, apply_defaults: bool) -> VmValue {
    schema_result_value_as(data, schema, apply_defaults, InputRepresentation::Json)
}

fn schema_result_value_as(
    data: &VmValue,
    schema: &VmValue,
    apply_defaults: bool,
    representation: InputRepresentation,
) -> VmValue {
    let normalized = match canonicalize_schema_value(schema) {
        Ok(schema) => schema,
        Err(error) => return result_err_value(vec![ValidationIssue::new("schema", error)], None),
    };
    let result = validate_schema_value_as(
        data,
        &normalized,
        ValidationOptions { apply_defaults },
        representation,
    );
    if result.errors.is_empty() {
        result_ok_value(result.value)
    } else {
        result_err_value(result.errors, Some(result.value))
    }
}

/// The `Err` that `schema_parse` returns, for a value that never reached
/// validation. `json_decode` uses it so a malformed document and a mistyped
/// one fail with the same record.
pub fn schema_result_err(path: &str, message: impl Into<String>) -> VmValue {
    result_err_value(vec![ValidationIssue::new(path, message)], None)
}

/// Return the first typed validation issue without projecting it through the
/// public `Result` value. Incremental validators use this boundary so their
/// machine-readable cause and human detail come from the same schema pass.
pub fn first_schema_validation_issue(
    data: &VmValue,
    schema: &VmValue,
) -> Option<SchemaValidationIssue> {
    let normalized = match canonicalize_schema_value(schema) {
        Ok(schema) => schema,
        Err(error) => {
            let issue = ValidationIssue::new("schema", error);
            return Some(project_validation_issue(issue));
        }
    };
    validate_schema_value(
        data,
        &normalized,
        ValidationOptions {
            apply_defaults: false,
        },
    )
    .errors
    .into_iter()
    .next()
    .map(project_validation_issue)
}

fn project_validation_issue(issue: ValidationIssue) -> SchemaValidationIssue {
    let detail = issue.render();
    SchemaValidationIssue {
        kind: issue.kind,
        detail,
        path: issue.path,
    }
}

pub fn schema_report_value(data: &VmValue, schema: &VmValue, apply_defaults: bool) -> VmValue {
    let normalized = match canonicalize_schema_value(schema) {
        Ok(schema) => schema,
        Err(error) => {
            return schema_report_from_issues(
                false,
                vec![ValidationIssue::new("schema", error)],
                None,
            )
        }
    };
    let result = validate_schema_value(data, &normalized, ValidationOptions { apply_defaults });
    let ok = result.errors.is_empty();
    schema_report_from_issues(ok, result.errors, Some(result.value))
}

fn schema_report_from_issues(
    ok: bool,
    issues: Vec<ValidationIssue>,
    value: Option<VmValue>,
) -> VmValue {
    let messages = issue_messages(&issues);
    let mut payload = BTreeMap::new();
    payload.put_bool("ok", ok);
    payload.put_str("message", messages.first().cloned().unwrap_or_default());
    payload.put("errors", VmValue::strings(messages));
    payload.insert("issues".to_string(), issue_values(&issues));
    if let Some(value) = value {
        payload.insert("value".to_string(), value);
    }
    VmValue::dict(payload)
}

pub fn schema_is_value(data: &VmValue, schema: &VmValue) -> Result<bool, VmError> {
    let normalized = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::string(error)))?;
    Ok(validate_schema_value(
        data,
        &normalized,
        ValidationOptions {
            apply_defaults: false,
        },
    )
    .errors
    .is_empty())
}

pub fn schema_expect_value(
    data: &VmValue,
    schema: &VmValue,
    apply_defaults: bool,
) -> Result<VmValue, VmError> {
    let normalized = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::string(error)))?;
    let result = validate_schema_value(data, &normalized, ValidationOptions { apply_defaults });
    if result.errors.is_empty() {
        Ok(result.value)
    } else {
        Err(VmError::Thrown(VmValue::string(
            issue_messages(&result.errors).join("; "),
        )))
    }
}

pub fn schema_assert_param(
    value: &VmValue,
    param_name: &str,
    schema: &VmValue,
) -> Result<(), VmError> {
    let normalized = cached_canonical_param_schema(schema)
        .map_err(|error| VmError::TypeError(format!("parameter '{param_name}': {error}")))?;
    schema_assert_canonical_param(value, param_name, &normalized)
}

pub fn schema_assert_canonical_param(
    value: &VmValue,
    param_name: &str,
    schema: &CanonicalParamSchema,
) -> Result<(), VmError> {
    let options = ValidationOptions {
        apply_defaults: false,
    };
    let schema_dict = schema.schema.as_ref();
    if let Some(error) =
        first_param_validation_error(value, schema_dict, schema_dict, param_name, options)
    {
        return Err(VmError::TypeError(error));
    }
    Ok(())
}

pub fn schema_to_json_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    let normalized = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::string(error)))?;
    let json_schema = canonical_to_json_schema(&normalized, false)
        .map_err(|error| VmError::Thrown(VmValue::string(error)))?;
    Ok(super::json_to_vm_value(&json_schema))
}

pub fn schema_to_openapi_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    let normalized = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::string(error)))?;
    let json_schema = canonical_to_json_schema(&normalized, true)
        .map_err(|error| VmError::Thrown(VmValue::string(error)))?;
    Ok(super::json_to_vm_value(&json_schema))
}

pub fn schema_from_json_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    canonicalize_schema_value(schema).map_err(|error| VmError::Thrown(VmValue::string(error)))
}

pub fn schema_from_openapi_schema_value(schema: &VmValue) -> Result<VmValue, VmError> {
    canonicalize_schema_value(schema).map_err(|error| VmError::Thrown(VmValue::string(error)))
}

pub fn schema_extend_value(base: &VmValue, overrides: &VmValue) -> Result<VmValue, VmError> {
    let base =
        canonicalize_schema_value(base).map_err(|error| VmError::Thrown(VmValue::string(error)))?;
    let overrides = canonicalize_schema_value(overrides)
        .map_err(|error| VmError::Thrown(VmValue::string(error)))?;
    let base_dict = base
        .as_dict()
        .ok_or_else(|| VmError::Thrown(VmValue::string("schema_extend: schema must be a dict")))?;
    let overrides_dict = overrides
        .as_dict()
        .ok_or_else(|| VmError::Thrown(VmValue::string("schema_extend: schema must be a dict")))?;
    Ok(VmValue::dict(merge_schema_dicts(base_dict, overrides_dict)))
}

pub fn schema_partial_value(schema: &VmValue) -> Result<VmValue, VmError> {
    let schema = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::string(error)))?;
    let schema_dict = schema
        .as_dict()
        .ok_or_else(|| VmError::Thrown(VmValue::string("schema_partial: schema must be a dict")))?;
    Ok(VmValue::dict(schema_partial_dict(schema_dict)))
}

pub fn schema_pick_value(schema: &VmValue, keys: &[String]) -> Result<VmValue, VmError> {
    let schema = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::string(error)))?;
    let schema_dict = schema
        .as_dict()
        .ok_or_else(|| VmError::Thrown(VmValue::string("schema_pick: schema must be a dict")))?;
    Ok(VmValue::dict(schema_pick_dict(schema_dict, keys)))
}

pub fn schema_omit_value(schema: &VmValue, keys: &[String]) -> Result<VmValue, VmError> {
    let schema = canonicalize_schema_value(schema)
        .map_err(|error| VmError::Thrown(VmValue::string(error)))?;
    let schema_dict = schema
        .as_dict()
        .ok_or_else(|| VmError::Thrown(VmValue::string("schema_omit: schema must be a dict")))?;
    Ok(VmValue::dict(schema_omit_dict(schema_dict, keys)))
}
