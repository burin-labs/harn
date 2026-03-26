use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Incoming DAP message (request or event from client).
#[derive(Debug, Deserialize)]
pub struct DapMessage {
    pub seq: i64,
    #[serde(rename = "type")]
    #[allow(dead_code)]
    pub msg_type: String,
    pub command: Option<String>,
    pub arguments: Option<Value>,
}

/// Outgoing DAP response or event.
#[derive(Debug, Serialize)]
pub struct DapResponse {
    pub seq: i64,
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_seq: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

impl DapResponse {
    pub fn success(seq: i64, request_seq: i64, command: &str, body: Option<Value>) -> Self {
        Self {
            seq,
            msg_type: "response".to_string(),
            request_seq: Some(request_seq),
            success: Some(true),
            command: Some(command.to_string()),
            message: None,
            body,
            event: None,
        }
    }

    pub fn event(seq: i64, event: &str, body: Option<Value>) -> Self {
        Self {
            seq,
            msg_type: "event".to_string(),
            request_seq: None,
            success: None,
            command: None,
            message: None,
            body,
            event: Some(event.to_string()),
        }
    }
}

/// DAP Capabilities.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub supports_configuration_done_request: bool,
    pub supports_evaluate_for_hovers: bool,
    pub supports_step_in_targets_request: bool,
    pub supports_set_variable: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            supports_configuration_done_request: true,
            supports_evaluate_for_hovers: true,
            supports_step_in_targets_request: false,
            supports_set_variable: false,
        }
    }
}

/// DAP Breakpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breakpoint {
    pub id: i64,
    pub verified: bool,
    pub line: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
}

/// DAP Source reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// DAP StackFrame.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    pub line: i64,
    pub column: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
}

/// DAP Scope.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub name: String,
    pub variables_reference: i64,
    pub expensive: bool,
}

/// DAP Variable.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub name: String,
    pub value: String,
    #[serde(rename = "type")]
    pub var_type: String,
    pub variables_reference: i64,
}
