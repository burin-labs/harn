use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

pub const TOOL_CALL_RECEIPT_SCHEMA_VERSION: u32 = 1;
pub const TOOL_CALL_RECEIPT_SCHEMA_ID: &str =
    "https://harnlang.com/schemas/tool-call-receipt.v1.json";
pub const TOOL_CALL_RECEIPT_SCHEMA_ARTIFACT: &str = "schemas/tool-call-receipt.schema.json";
pub const TOOL_CALL_RECEIPT_STATUSES: &[&str] = &[
    "ok",
    "schema_violation",
    "consent_denied",
    "timeout",
    "error",
];
pub const TOOL_CALL_RECEIPT_EXECUTORS: &[&str] =
    &["harn", "host_bridge", "mcp_server", "provider_native"];

/// Typed receipt emitted for each audited tool call.
///
/// Receipts intentionally avoid raw arguments and raw results. Callers get
/// stable hashes for correlation, plus free-form audit metadata for
/// user-visible rationale and middleware decisions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCallReceipt {
    pub schema_version: u32,
    pub session_id: String,
    pub run_id: Option<String>,
    pub tool_call_id: String,
    pub tool_name: String,
    pub iteration: u64,
    pub turn_index: Option<u64>,
    pub reason: Option<String>,
    pub kind: Option<String>,
    pub executor: Option<String>,
    pub status: String,
    pub error_category: Option<String>,
    pub duration_ms: u64,
    pub args_hash: String,
    pub result_hash: Option<String>,
    pub audit: JsonValue,
    pub emitted_at: String,
    pub model: Option<String>,
    pub provider: Option<String>,
}

pub fn tool_call_receipt_schema() -> JsonValue {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": TOOL_CALL_RECEIPT_SCHEMA_ID,
        "title": "ToolCallReceipt",
        "type": "object",
        "additionalProperties": false,
        "required": [
            "schema_version",
            "session_id",
            "run_id",
            "tool_call_id",
            "tool_name",
            "iteration",
            "turn_index",
            "reason",
            "kind",
            "executor",
            "status",
            "error_category",
            "duration_ms",
            "args_hash",
            "result_hash",
            "audit",
            "emitted_at",
            "model",
            "provider"
        ],
        "properties": {
            "schema_version": {
                "type": "integer",
                "const": TOOL_CALL_RECEIPT_SCHEMA_VERSION,
                "description": "Receipt schema version. Starts at 1."
            },
            "session_id": {"type": "string", "minLength": 1},
            "run_id": nullable_string(),
            "tool_call_id": {"type": "string", "minLength": 1},
            "tool_name": {"type": "string", "minLength": 1},
            "iteration": {"type": "integer", "minimum": 0},
            "turn_index": {"type": ["integer", "null"], "minimum": 0},
            "reason": nullable_string(),
            "kind": nullable_string(),
            "executor": {
                "type": ["string", "null"],
                "enum": executor_schema_enum(),
            },
            "status": {
                "type": "string",
                "enum": TOOL_CALL_RECEIPT_STATUSES,
            },
            "error_category": nullable_string(),
            "duration_ms": {"type": "integer", "minimum": 0},
            "args_hash": digest_schema(),
            "result_hash": {
                "anyOf": [
                    digest_schema(),
                    {"type": "null"}
                ],
            },
            "audit": true,
            "emitted_at": {
                "type": "string",
                "format": "date-time",
            },
            "model": nullable_string(),
            "provider": nullable_string(),
        },
        "x-harn-provenance": {
            "source": "crates/harn-vm/src/llm/receipts.rs",
            "schemaVersion": TOOL_CALL_RECEIPT_SCHEMA_VERSION
        }
    })
}

fn nullable_string() -> JsonValue {
    json!({"type": ["string", "null"]})
}

fn digest_schema() -> JsonValue {
    json!({
        "type": "string",
        "pattern": "^[a-f0-9]{64}$",
    })
}

fn executor_schema_enum() -> JsonValue {
    let mut values: Vec<JsonValue> = TOOL_CALL_RECEIPT_EXECUTORS
        .iter()
        .map(|value| JsonValue::String((*value).to_string()))
        .collect();
    values.push(JsonValue::Null);
    JsonValue::Array(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_receipt() -> ToolCallReceipt {
        ToolCallReceipt {
            schema_version: TOOL_CALL_RECEIPT_SCHEMA_VERSION,
            session_id: "session-1".to_string(),
            run_id: Some("run-1".to_string()),
            tool_call_id: "tool-1".to_string(),
            tool_name: "read_file".to_string(),
            iteration: 2,
            turn_index: Some(1),
            reason: Some("Read project context".to_string()),
            kind: Some("read".to_string()),
            executor: Some("harn".to_string()),
            status: "ok".to_string(),
            error_category: None,
            duration_ms: 7,
            args_hash: "0".repeat(64),
            result_hash: Some("1".repeat(64)),
            audit: json!({"summary": "Read project context"}),
            emitted_at: "2026-05-16T00:00:00Z".to_string(),
            model: Some("mock".to_string()),
            provider: Some("mock".to_string()),
        }
    }

    #[test]
    fn tool_call_receipt_serializes_nullable_optional_fields() {
        let mut receipt = fixture_receipt();
        receipt.run_id = None;
        receipt.turn_index = None;
        receipt.result_hash = None;
        let value = serde_json::to_value(&receipt).expect("receipt serializes");
        assert_eq!(value["schema_version"], json!(1));
        assert_eq!(value["run_id"], JsonValue::Null);
        assert_eq!(value["turn_index"], JsonValue::Null);
        assert_eq!(value["result_hash"], JsonValue::Null);

        let round_trip: ToolCallReceipt =
            serde_json::from_value(value).expect("receipt deserializes");
        assert_eq!(round_trip, receipt);
    }

    #[test]
    fn schema_pins_statuses_and_privacy_hash_fields() {
        let schema = tool_call_receipt_schema();
        assert_eq!(
            schema["properties"]["status"]["enum"],
            json!(TOOL_CALL_RECEIPT_STATUSES)
        );
        assert_eq!(
            schema["properties"]["args_hash"]["pattern"],
            json!("^[a-f0-9]{64}$")
        );
        assert_eq!(
            schema["x-harn-provenance"]["source"],
            json!("crates/harn-vm/src/llm/receipts.rs")
        );
    }
}
