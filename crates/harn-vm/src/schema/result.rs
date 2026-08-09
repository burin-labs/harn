use crate::value::VmDictExt;
use std::collections::BTreeMap;

use crate::value::VmValue;

#[derive(Debug, Clone)]
pub(super) struct ValidationResult {
    pub(super) value: VmValue,
    pub(super) errors: Vec<ValidationIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ValidationIssue {
    pub(super) path: String,
    pub(super) message: String,
    pub(super) code: &'static str,
}

impl ValidationIssue {
    pub(super) fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
            code: "schema.validation",
        }
    }

    pub(super) fn schema(path: &str, message: impl Into<String>) -> Self {
        Self::new(location_label(path), message)
    }

    pub(super) fn render(&self) -> String {
        format!("at {}: {}", self.path, self.message)
    }

    pub(super) fn to_vm_value(&self) -> VmValue {
        let mut payload = BTreeMap::new();
        payload.put_str("path", self.path.clone());
        payload.put_str("message", self.message.clone());
        payload.put_str("code", self.code);
        VmValue::dict(payload)
    }
}

pub(super) fn location_label(path: &str) -> String {
    if path.is_empty() {
        "root".to_string()
    } else {
        path.to_string()
    }
}

pub(super) fn issue_messages(issues: &[ValidationIssue]) -> Vec<String> {
    issues.iter().map(ValidationIssue::render).collect()
}

pub(super) fn issue_values(issues: &[ValidationIssue]) -> VmValue {
    VmValue::List(std::sync::Arc::new(
        issues.iter().map(ValidationIssue::to_vm_value).collect(),
    ))
}

pub(super) fn result_ok_value(value: VmValue) -> VmValue {
    VmValue::enum_variant("Result", "Ok", vec![value])
}

pub(super) fn result_err_value(errors: Vec<ValidationIssue>, value: Option<VmValue>) -> VmValue {
    let mut payload = BTreeMap::new();
    let messages = issue_messages(&errors);
    payload.put_str(
        "message",
        messages
            .first()
            .cloned()
            .unwrap_or_else(|| "schema validation failed".to_string()),
    );
    payload.insert("issues".to_string(), issue_values(&errors));
    payload.insert(
        "errors".to_string(),
        VmValue::List(std::sync::Arc::new(
            messages
                .into_iter()
                .map(|err| VmValue::String(arcstr::ArcStr::from(err)))
                .collect(),
        )),
    );
    if let Some(value) = value {
        payload.insert("value".to_string(), value);
    }
    VmValue::enum_variant("Result", "Err", vec![VmValue::dict(payload)])
}
