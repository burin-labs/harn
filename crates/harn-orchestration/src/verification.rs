//! Verification requirements normalized from resolved workflow inputs.

use harn_vm_core::VmError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VerificationRequirement {
    pub kind: String,
    pub value: String,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct VerificationContract {
    pub source_node: Option<String>,
    pub summary: Option<String>,
    pub command: Option<String>,
    pub expect_status: Option<i64>,
    pub assert_text: Option<String>,
    pub expect_text: Option<String>,
    pub required_identifiers: Vec<String>,
    pub required_paths: Vec<String>,
    pub required_text: Vec<String>,
    pub notes: Vec<String>,
    pub checks: Vec<VerificationRequirement>,
}

impl VerificationContract {
    fn is_empty(&self) -> bool {
        self.summary.is_none()
            && self.command.is_none()
            && self.expect_status.is_none()
            && self.assert_text.is_none()
            && self.expect_text.is_none()
            && self.required_identifiers.is_empty()
            && self.required_paths.is_empty()
            && self.required_text.is_empty()
            && self.notes.is_empty()
            && self.checks.is_empty()
    }
}

fn push_unique_string(values: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    if !values.iter().any(|existing| existing == trimmed) {
        values.push(trimmed.to_string());
    }
}

fn push_unique_requirement(
    values: &mut Vec<VerificationRequirement>,
    kind: &str,
    value: &str,
    note: Option<&str>,
) {
    let trimmed_kind = kind.trim();
    let trimmed_value = value.trim();
    let trimmed_note = note
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(|candidate| candidate.to_string());
    if trimmed_kind.is_empty() || trimmed_value.is_empty() {
        return;
    }
    let candidate = VerificationRequirement {
        kind: trimmed_kind.to_string(),
        value: trimmed_value.to_string(),
        note: trimmed_note,
    };
    if !values.iter().any(|existing| existing == &candidate) {
        values.push(candidate);
    }
}

fn json_string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(text)) => {
            let mut values = Vec::new();
            push_unique_string(&mut values, text);
            values
        }
        Some(Value::Array(items)) => {
            let mut values = Vec::new();
            for item in items {
                if let Some(text) = item.as_str() {
                    push_unique_string(&mut values, text);
                }
            }
            values
        }
        _ => Vec::new(),
    }
}

fn merge_verification_requirement_list(
    target: &mut Vec<VerificationRequirement>,
    value: Option<&Value>,
) {
    let Some(items) = value.and_then(|raw| raw.as_array()) else {
        return;
    };
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let kind = object
            .get("kind")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let value = object
            .get("value")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let note = object
            .get("note")
            .or_else(|| object.get("description"))
            .or_else(|| object.get("reason"))
            .and_then(|value| value.as_str());
        push_unique_requirement(target, kind, value, note);
    }
}

fn merge_verification_contract_fields(
    target: &mut VerificationContract,
    object: &Map<String, Value>,
) {
    if target.summary.is_none() {
        target.summary = object
            .get("summary")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
    }
    if target.command.is_none() {
        target.command = object
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
    }
    if target.expect_status.is_none() {
        target.expect_status = object.get("expect_status").and_then(|value| value.as_i64());
    }
    if target.assert_text.is_none() {
        target.assert_text = object
            .get("assert_text")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
    }
    if target.expect_text.is_none() {
        target.expect_text = object
            .get("expect_text")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());
    }

    for value in json_string_list(
        object
            .get("required_identifiers")
            .or_else(|| object.get("identifiers")),
    ) {
        push_unique_string(&mut target.required_identifiers, &value);
    }
    for value in json_string_list(object.get("required_paths").or_else(|| object.get("paths"))) {
        push_unique_string(&mut target.required_paths, &value);
    }
    for value in json_string_list(
        object
            .get("required_text")
            .or_else(|| object.get("exact_text"))
            .or_else(|| object.get("required_strings")),
    ) {
        push_unique_string(&mut target.required_text, &value);
    }
    for value in json_string_list(object.get("notes")) {
        push_unique_string(&mut target.notes, &value);
    }
    merge_verification_requirement_list(&mut target.checks, object.get("checks"));
}

/// Compose file, inline contract, and verify fields in that precedence order.
/// The host resolves file contents before entering this pure boundary.
pub fn verification_contract_from_sources(
    node_id: &str,
    verify: &Map<String, Value>,
    file_contract: Option<&Value>,
) -> Result<Option<VerificationContract>, VmError> {
    let mut contract = VerificationContract {
        source_node: Some(node_id.to_string()),
        ..Default::default()
    };

    if let Some(file_contract) = file_contract {
        let Some(object) = file_contract.as_object() else {
            return Err(VmError::Runtime(
                "workflow verification contract file must parse to a JSON object".to_string(),
            ));
        };
        merge_verification_contract_fields(&mut contract, object);
    }

    if let Some(inline_contract) = verify.get("contract") {
        let Some(object) = inline_contract.as_object() else {
            return Err(VmError::Runtime(
                "workflow verify.contract must be an object".to_string(),
            ));
        };
        merge_verification_contract_fields(&mut contract, object);
    }

    merge_verification_contract_fields(&mut contract, verify);

    if let Some(assert_text) = contract.assert_text.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "visible_text_contains",
            &assert_text,
            Some("verify stage requires visible output to contain this text"),
        );
    }
    if let Some(expect_text) = contract.expect_text.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "combined_output_contains",
            &expect_text,
            Some("verify command requires combined stdout/stderr to contain this text"),
        );
    }
    if let Some(expect_status) = contract.expect_status {
        push_unique_requirement(
            &mut contract.checks,
            "expect_status",
            &expect_status.to_string(),
            Some("verify command exit status must match exactly"),
        );
    }
    for identifier in contract.required_identifiers.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "identifier",
            &identifier,
            Some("use this exact identifier spelling"),
        );
    }
    for path in contract.required_paths.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "path",
            &path,
            Some("preserve this exact path"),
        );
    }
    for text in contract.required_text.clone() {
        push_unique_requirement(
            &mut contract.checks,
            "text",
            &text,
            Some("required exact text or wiring snippet"),
        );
    }

    if contract.is_empty() {
        return Ok(None);
    }
    Ok(Some(contract))
}

#[cfg(test)]
mod tests;
