use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;

pub const RECEIPT_SCHEMA_ID: &str = "https://harnlang.com/schemas/receipt.v1.json";
pub const RECEIPT_SCHEMA_VERSION: &str = "harn.receipt.v1";
pub const RECEIPT_SCHEMA_JSON: &str = include_str!("../../../../docs/schemas/receipt.v1.json");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Receipt {
    pub schema: String,
    pub id: String,
    pub parent_run_id: Option<String>,
    pub persona: String,
    pub step: Option<String>,
    pub trace_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
    pub status: ReceiptStatus,
    pub inputs_digest: Option<String>,
    pub outputs_digest: Option<String>,
    pub model_calls: Vec<BTreeMap<String, JsonValue>>,
    pub tool_calls: Vec<BTreeMap<String, JsonValue>>,
    pub cost_usd: f64,
    pub approvals: Vec<BTreeMap<String, JsonValue>>,
    pub handoffs: Vec<BTreeMap<String, JsonValue>>,
    pub side_effects: Vec<BTreeMap<String, JsonValue>>,
    pub error: Option<BTreeMap<String, JsonValue>>,
    pub redaction_class: RedactionClass,
    pub metadata: BTreeMap<String, JsonValue>,
}

impl Default for Receipt {
    fn default() -> Self {
        Self {
            schema: RECEIPT_SCHEMA_VERSION.to_string(),
            id: String::new(),
            parent_run_id: None,
            persona: String::new(),
            step: None,
            trace_id: String::new(),
            started_at: OffsetDateTime::from_unix_timestamp(0).unwrap(),
            completed_at: None,
            status: ReceiptStatus::default(),
            inputs_digest: None,
            outputs_digest: None,
            model_calls: Vec::new(),
            tool_calls: Vec::new(),
            cost_usd: 0.0,
            approvals: Vec::new(),
            handoffs: Vec::new(),
            side_effects: Vec::new(),
            error: None,
            redaction_class: RedactionClass::default(),
            metadata: BTreeMap::new(),
        }
    }
}

impl Receipt {
    pub fn new(
        id: impl Into<String>,
        persona: impl Into<String>,
        trace_id: impl Into<String>,
        started_at: OffsetDateTime,
    ) -> Self {
        Self {
            schema: RECEIPT_SCHEMA_VERSION.to_string(),
            id: id.into(),
            persona: persona.into(),
            trace_id: trace_id.into(),
            started_at,
            status: ReceiptStatus::Running,
            redaction_class: RedactionClass::Internal,
            ..Self::default()
        }
    }

    pub fn completed(mut self, completed_at: OffsetDateTime, status: ReceiptStatus) -> Self {
        self.completed_at = Some(completed_at);
        self.status = status;
        self
    }

    pub fn validate_required_shape(&self) -> Result<(), ReceiptValidationError> {
        if self.schema != RECEIPT_SCHEMA_VERSION {
            return Err(ReceiptValidationError::InvalidSchema(self.schema.clone()));
        }
        if self.id.trim().is_empty() {
            return Err(ReceiptValidationError::MissingField("id"));
        }
        if self.persona.trim().is_empty() {
            return Err(ReceiptValidationError::MissingField("persona"));
        }
        if self.trace_id.trim().is_empty() {
            return Err(ReceiptValidationError::MissingField("trace_id"));
        }
        if !self.cost_usd.is_finite() || self.cost_usd < 0.0 {
            return Err(ReceiptValidationError::InvalidCost(self.cost_usd));
        }
        Ok(())
    }

    pub fn schema_json() -> Result<JsonValue, serde_json::Error> {
        serde_json::from_str(RECEIPT_SCHEMA_JSON)
    }
}

#[async_trait]
pub trait ReceiptSink {
    type Error;

    async fn persist_receipt(&self, receipt: &Receipt) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Accepted,
    #[default]
    Running,
    Success,
    Noop,
    Failure,
    Denied,
    Duplicate,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionClass {
    Public,
    #[default]
    Internal,
    ReceiptOnly,
    Secret,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReceiptValidationError {
    InvalidSchema(String),
    MissingField(&'static str),
    InvalidCost(f64),
}

impl std::fmt::Display for ReceiptValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReceiptValidationError::InvalidSchema(schema) => {
                write!(f, "unsupported receipt schema `{schema}`")
            }
            ReceiptValidationError::MissingField(field) => {
                write!(f, "receipt is missing required field `{field}`")
            }
            ReceiptValidationError::InvalidCost(cost) => {
                write!(
                    f,
                    "receipt cost_usd must be finite and non-negative, got {cost}"
                )
            }
        }
    }
}

impl std::error::Error for ReceiptValidationError {}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_receipt() -> Receipt {
        Receipt::new(
            "receipt_01JZCANONICAL",
            "merge_captain",
            "trace_01JZCANONICAL",
            OffsetDateTime::from_unix_timestamp(1_777_000_000).unwrap(),
        )
        .completed(
            OffsetDateTime::from_unix_timestamp(1_777_000_030).unwrap(),
            ReceiptStatus::Success,
        )
    }

    #[test]
    fn receipt_new_serializes_with_canonical_defaults() {
        let value = serde_json::to_value(Receipt::new(
            "receipt_1",
            "persona",
            "trace_1",
            OffsetDateTime::from_unix_timestamp(1_777_000_000).unwrap(),
        ))
        .unwrap();

        assert_eq!(value["schema"], RECEIPT_SCHEMA_VERSION);
        assert_eq!(value["status"], "running");
        assert_eq!(value["redaction_class"], "internal");
        assert!(value["model_calls"].as_array().unwrap().is_empty());
        assert!(value["tool_calls"].as_array().unwrap().is_empty());
        assert!(value["approvals"].as_array().unwrap().is_empty());
        assert!(value["handoffs"].as_array().unwrap().is_empty());
        assert!(value["side_effects"].as_array().unwrap().is_empty());
    }

    #[test]
    fn receipt_to_value_matches_published_schema_surface() {
        let receipt_value = serde_json::to_value(fixture_receipt()).unwrap();
        let receipt_object = receipt_value.as_object().unwrap();
        let schema = Receipt::schema_json().unwrap();
        let schema_object = schema.as_object().unwrap();
        let properties = schema_object["properties"].as_object().unwrap();

        for required in schema_object["required"].as_array().unwrap() {
            let key = required.as_str().unwrap();
            assert!(
                receipt_object.contains_key(key),
                "serialized receipt is missing schema-required key `{key}`"
            );
        }

        for key in receipt_object.keys() {
            assert!(
                properties.contains_key(key),
                "serialized receipt has key `{key}` not published in docs/schemas/receipt.v1.json"
            );
        }

        assert_eq!(schema_object["$id"], RECEIPT_SCHEMA_ID);
        assert_eq!(properties["schema"]["const"], json!(RECEIPT_SCHEMA_VERSION));
        assert!(properties["status"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("success")));
        assert!(properties["redaction_class"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("receipt_only")));
    }

    #[test]
    fn receipt_shape_validation_rejects_bad_core_fields() {
        let mut receipt = fixture_receipt();
        assert!(receipt.validate_required_shape().is_ok());

        receipt.trace_id.clear();
        assert_eq!(
            receipt.validate_required_shape(),
            Err(ReceiptValidationError::MissingField("trace_id"))
        );

        receipt.trace_id = "trace_ok".to_string();
        receipt.cost_usd = -0.01;
        assert_eq!(
            receipt.validate_required_shape(),
            Err(ReceiptValidationError::InvalidCost(-0.01))
        );
    }
}
