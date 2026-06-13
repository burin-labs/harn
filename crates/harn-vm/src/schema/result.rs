use crate::value::VmDictExt;
use std::collections::BTreeMap;

use crate::value::VmValue;

#[derive(Debug, Clone)]
pub(super) struct ValidationResult {
    pub(super) value: VmValue,
    pub(super) errors: Vec<String>,
}

pub(super) fn result_ok_value(value: VmValue) -> VmValue {
    VmValue::enum_variant("Result", "Ok", vec![value])
}

pub(super) fn result_err_value(errors: Vec<String>, value: Option<VmValue>) -> VmValue {
    let mut payload = BTreeMap::new();
    payload.put_str(
        "message",
        errors
            .first()
            .cloned()
            .unwrap_or_else(|| "schema validation failed".to_string()),
    );
    payload.insert(
        "errors".to_string(),
        VmValue::List(std::sync::Arc::new(
            errors
                .into_iter()
                .map(|err| VmValue::String(std::sync::Arc::from(err)))
                .collect(),
        )),
    );
    if let Some(value) = value {
        payload.insert("value".to_string(), value);
    }
    VmValue::enum_variant("Result", "Err", vec![VmValue::dict(payload)])
}
