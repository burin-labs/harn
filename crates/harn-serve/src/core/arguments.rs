use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use harn_vm::VmValue;

use crate::{CallArguments, DispatchError};

pub(super) fn build_vm_args(
    arguments: &CallArguments,
    function: &crate::ExportedFunction,
) -> Result<Vec<VmValue>, DispatchError> {
    // ExportCatalog removes the contiguous host-injected authority prefix.
    // The remaining parameters are exactly the caller-owned JSON API.
    let params = function.params.as_slice();
    let rest = match arguments {
        CallArguments::Positional(values) => values.iter().map(json_to_vm_value).collect(),
        CallArguments::Named(values) => {
            let lifted = lift_flat_single_object_arg(params, values);
            let values = lifted.as_ref().unwrap_or(values);
            let mut args = Vec::new();
            let mut saw_gap = false;
            for param in params {
                let value = values.get(&param.name);
                if param.rest {
                    if let Some(value) = value {
                        let rest = value.as_array().ok_or_else(|| {
                            DispatchError::Validation(format!(
                                "rest argument '{}' for '{}' must be an array",
                                param.name, function.name
                            ))
                        })?;
                        args.extend(rest.iter().map(json_to_vm_value));
                    }
                    continue;
                }
                match value {
                    Some(value) => {
                        if saw_gap {
                            return Err(DispatchError::Validation(format!(
                                "named arguments for '{}' skipped '{}' before later arguments",
                                function.name, param.name
                            )));
                        }
                        args.push(json_to_vm_value(value));
                    }
                    None if param.has_default => saw_gap = true,
                    None => {
                        return Err(DispatchError::Validation(format!(
                            "missing required argument '{}' for '{}'",
                            param.name, function.name
                        )));
                    }
                }
            }
            trim_trailing_defaults(args)
        }
    };
    Ok(rest)
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

fn trim_trailing_defaults(args: Vec<VmValue>) -> Vec<VmValue> {
    let mut tail = VecDeque::from(args);
    while matches!(tail.back(), Some(VmValue::Nil)) {
        tail.pop_back();
    }
    tail.into_iter().collect()
}

fn json_to_vm_value(value: &serde_json::Value) -> VmValue {
    match value {
        serde_json::Value::Null => VmValue::Nil,
        serde_json::Value::Bool(value) => VmValue::Bool(*value),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(VmValue::Int)
            .or_else(|| value.as_f64().map(VmValue::Float))
            .unwrap_or(VmValue::Nil),
        serde_json::Value::String(value) => VmValue::String(arcstr::ArcStr::from(value.as_str())),
        serde_json::Value::Array(items) => VmValue::List(Arc::new(
            items.iter().map(json_to_vm_value).collect::<Vec<_>>(),
        )),
        serde_json::Value::Object(map) => VmValue::dict(
            map.iter()
                .map(|(key, value)| (key.clone(), json_to_vm_value(value)))
                .collect::<harn_vm::value::DictMap>(),
        ),
    }
}
