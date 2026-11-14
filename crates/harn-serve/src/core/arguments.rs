use std::collections::BTreeMap;

use harn_vm::VmValue;

use crate::{CallArguments, DispatchError};

pub(super) fn build_vm_args(
    arguments: &CallArguments,
    function: &crate::ExportedFunction,
    contract: &harn_vm::tool_registry::PreparedToolCatalog,
) -> Result<Vec<VmValue>, DispatchError> {
    // ExportCatalog removes the contiguous host-injected authority prefix.
    // The remaining parameters are exactly the caller-owned JSON API.
    let params = function.params.as_slice();
    let canonical = canonical_arguments_json(arguments, function)?;
    let decoded = contract
        .decode_input(&function.name, &canonical)
        .map_err(DispatchError::Contract)?;
    let values = decoded.as_dict().ok_or_else(|| {
        DispatchError::Validation("export argument contract must decode to a named record".into())
    })?;
    let mut args = Vec::new();
    let mut saw_gap = false;
    for param in params {
        match values.get(param.name.as_str()) {
            Some(VmValue::List(values)) if param.rest => args.extend(values.iter().cloned()),
            Some(_) if param.rest => {
                return Err(DispatchError::Validation(
                    "rest argument contract must decode to a list".into(),
                ))
            }
            None if param.rest => {}
            Some(value) => {
                if saw_gap {
                    return Err(DispatchError::Validation(format!(
                        "named arguments for '{}' skipped '{}' before later arguments",
                        function.name, param.name
                    )));
                }
                args.push(value.clone());
            }
            None if param.has_default => saw_gap = true,
            None => {
                return Err(DispatchError::Validation(format!(
                    "missing required argument '{}' for '{}'",
                    param.name, function.name
                )))
            }
        }
    }
    Ok(args)
}

/// Normalize every adapter's arguments into the advertised input-schema object.
pub(super) fn canonical_arguments_json(
    arguments: &CallArguments,
    function: &crate::ExportedFunction,
) -> Result<serde_json::Value, DispatchError> {
    let params = function.params.as_slice();
    let values = match arguments {
        CallArguments::Named(values) => {
            lift_flat_single_object_arg(params, values).unwrap_or_else(|| values.clone())
        }
        CallArguments::Positional(values) => {
            let rest_index = params.iter().position(|param| param.rest);
            if values.len() > params.len() && rest_index.is_none() {
                return Err(DispatchError::Validation(format!(
                    "too many positional arguments for '{}': expected at most {}, got {}",
                    function.name,
                    params.len(),
                    values.len()
                )));
            }
            let mut arguments = BTreeMap::new();
            for (index, param) in params.iter().enumerate() {
                if param.rest {
                    arguments.insert(
                        param.name.clone(),
                        serde_json::Value::Array(values.get(index..).unwrap_or_default().to_vec()),
                    );
                    break;
                }
                if let Some(value) = values.get(index) {
                    arguments.insert(param.name.clone(), value.clone());
                }
            }
            arguments
        }
    };
    Ok(serde_json::Value::Object(values.into_iter().collect()))
}

/// Lift flat input into a single object parameter when no declared name binds.
/// Correctly nested, empty, scalar, variadic, and multi-parameter calls remain
/// unchanged, so this compatibility normalization is idempotent and additive.
pub(super) fn lift_flat_single_object_arg(
    params: &[crate::ExportedParam],
    values: &BTreeMap<String, serde_json::Value>,
) -> Option<BTreeMap<String, serde_json::Value>> {
    let [only] = params else {
        return None;
    };
    if only.rest || !only.accepts_json_object() {
        return None;
    }
    if values.is_empty() || values.contains_key(&only.name) {
        return None;
    }
    let wrapped = serde_json::Value::Object(values.clone().into_iter().collect());
    Some(BTreeMap::from([(only.name.clone(), wrapped)]))
}
