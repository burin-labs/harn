use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;

pub const RECEIPT_SCHEMA_ID: &str = "https://harnlang.com/schemas/receipt.v1.json";
pub const RECEIPT_SCHEMA_VERSION: &str = "harn.receipt.v1";
pub const RECEIPT_SCHEMA_JSON: &str = include_str!("receipt.v1.json");

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

    /// Append a `model_calls[]` entry that records the per-step model
    /// + token + cost breakdown produced by `crates/harn-vm/src/step_runtime.rs`.
    ///   Used by run-receipt builders (a cloud store, an IDE host) so a
    ///   single canonical envelope carries per-step economics without
    ///   each consumer reinventing the field layout.
    pub fn push_step_breakdown(&mut self, summary: &crate::step_runtime::CompletedStep) {
        let mut entry: BTreeMap<String, JsonValue> = BTreeMap::new();
        entry.insert("step".to_string(), JsonValue::String(summary.name.clone()));
        entry.insert(
            "function".to_string(),
            JsonValue::String(summary.function.clone()),
        );
        if let Some(model) = summary.model.as_deref() {
            entry.insert("model".to_string(), JsonValue::String(model.to_string()));
        }
        entry.insert(
            "input_tokens".to_string(),
            JsonValue::Number(summary.input_tokens.into()),
        );
        entry.insert(
            "output_tokens".to_string(),
            JsonValue::Number(summary.output_tokens.into()),
        );
        entry.insert(
            "llm_calls".to_string(),
            JsonValue::Number(summary.llm_calls.into()),
        );
        entry.insert(
            "status".to_string(),
            JsonValue::String(summary.status.clone()),
        );
        if let Some(error) = summary.error.as_deref() {
            entry.insert("error".to_string(), JsonValue::String(error.to_string()));
        }
        if summary.cost_usd.is_finite() {
            if let Some(num) = serde_json::Number::from_f64(summary.cost_usd) {
                entry.insert("cost_usd".to_string(), JsonValue::Number(num));
            }
            self.cost_usd += summary.cost_usd;
        }
        self.model_calls.push(entry);
    }

    /// Drain the per-thread step log into this receipt's `model_calls[]`
    /// in declaration order. Idempotent: a second call after the
    /// thread-local has been drained appends nothing.
    pub fn attach_completed_steps(&mut self) {
        for summary in crate::step_runtime::drain_completed_steps() {
            self.push_step_breakdown(&summary);
        }
    }

    /// Apply the unified [`crate::redact::RedactionPolicy`] to every
    /// caller-supplied JSON field on the receipt. Fixed envelope fields
    /// (id, persona, trace_id, status, schema, digests, cost) are not
    /// touched — the persistence layer needs them stable for query and
    /// replay.
    pub fn redact_in_place(&mut self, policy: &crate::redact::RedactionPolicy) {
        fn redact_entries(
            entries: &mut [BTreeMap<String, JsonValue>],
            policy: &crate::redact::RedactionPolicy,
        ) {
            for entry in entries {
                for (key, value) in entry.iter_mut() {
                    if policy.field_is_sensitive(key) {
                        *value = JsonValue::String(crate::redact::REDACTED_PLACEHOLDER.to_string());
                    } else {
                        policy.redact_json_in_place(value);
                    }
                }
            }
        }

        redact_entries(&mut self.model_calls, policy);
        redact_entries(&mut self.tool_calls, policy);
        redact_entries(&mut self.approvals, policy);
        redact_entries(&mut self.handoffs, policy);
        redact_entries(&mut self.side_effects, policy);
        if let Some(error) = self.error.as_mut() {
            for (key, value) in error.iter_mut() {
                if policy.field_is_sensitive(key) {
                    *value = JsonValue::String(crate::redact::REDACTED_PLACEHOLDER.to_string());
                } else {
                    policy.redact_json_in_place(value);
                }
            }
        }
        for (key, value) in self.metadata.iter_mut() {
            if policy.field_is_sensitive(key) {
                *value = JsonValue::String(crate::redact::REDACTED_PLACEHOLDER.to_string());
            } else {
                policy.redact_json_in_place(value);
            }
        }
    }
}

#[async_trait]
pub trait ReceiptSink {
    type Error;

    async fn persist_receipt(&self, receipt: &Receipt) -> Result<(), Self::Error>;
}

/// `ReceiptSink` decorator that runs each receipt through
/// [`Receipt::redact_in_place`] before delegating to the inner sink.
/// Hosts can wrap their existing sinks to opt every persistence path
/// into the unified redaction policy without touching call sites that
/// build the receipts.
pub struct RedactingReceiptSink<Inner> {
    inner: Inner,
    policy: crate::redact::RedactionPolicy,
}

impl<Inner> RedactingReceiptSink<Inner> {
    pub fn new(inner: Inner, policy: crate::redact::RedactionPolicy) -> Self {
        Self { inner, policy }
    }

    /// Convenience constructor that wraps `inner` with the currently
    /// installed thread-local policy.
    pub fn with_current_policy(inner: Inner) -> Self {
        Self::new(inner, crate::redact::current_policy())
    }
}

#[async_trait]
impl<Inner> ReceiptSink for RedactingReceiptSink<Inner>
where
    Inner: ReceiptSink + Send + Sync,
{
    type Error = Inner::Error;

    async fn persist_receipt(&self, receipt: &Receipt) -> Result<(), Self::Error> {
        let mut redacted = receipt.clone();
        redacted.redact_in_place(&self.policy);
        self.inner.persist_receipt(&redacted).await
    }
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
    fn embedded_receipt_schema_matches_workspace_docs_when_available() {
        let docs_schema = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/schemas/receipt.v1.json");
        if !docs_schema.exists() {
            return;
        }
        let source = std::fs::read_to_string(&docs_schema)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", docs_schema.display()));
        assert_eq!(
            RECEIPT_SCHEMA_JSON, source,
            "embedded receipt schema drifted from docs/schemas/receipt.v1.json"
        );
    }

    #[test]
    fn receipt_attaches_per_step_breakdown_with_aggregated_cost() {
        let summary = crate::step_runtime::CompletedStep {
            name: "classify".to_string(),
            function: "classify_step".to_string(),
            model: Some("claude-haiku-4-5".to_string()),
            input_tokens: 5,
            output_tokens: 5,
            cost_usd: 0.000_05,
            llm_calls: 1,
            status: "completed".to_string(),
            error: None,
        };
        let mut receipt = fixture_receipt();
        let starting_cost = receipt.cost_usd;
        receipt.push_step_breakdown(&summary);
        assert_eq!(receipt.model_calls.len(), 1);
        let entry = &receipt.model_calls[0];
        assert_eq!(entry["step"], json!("classify"));
        assert_eq!(entry["function"], json!("classify_step"));
        assert_eq!(entry["model"], json!("claude-haiku-4-5"));
        assert_eq!(entry["input_tokens"], json!(5));
        assert_eq!(entry["output_tokens"], json!(5));
        assert_eq!(entry["llm_calls"], json!(1));
        assert!((receipt.cost_usd - starting_cost - 0.000_05).abs() < 1e-9);
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
