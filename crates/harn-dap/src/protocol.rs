use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Incoming DAP message. Carries the union of request (`type: "request"`)
/// and response (`type: "response"`) shapes — incoming responses are how
/// the client replies to *reverse requests* the adapter sent (DAP's
/// canonical mechanism for adapter→client RPCs, à la `runInTerminal`).
#[derive(Debug, Deserialize)]
pub struct DapMessage {
    pub seq: i64,
    #[serde(rename = "type")]
    pub msg_type: String,
    pub command: Option<String>,
    pub arguments: Option<Value>,
    pub request_seq: Option<i64>,
    pub success: Option<bool>,
    pub message: Option<String>,
    pub body: Option<Value>,
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

/// DAP Capabilities. `supports_harn_host_call` is a Harn-specific
/// extension key that signals the client implements the `harnHostCall`
/// reverse request — when present, the adapter routes any unhandled
/// `host_call` op back to the client instead of the harn-vm fallback
/// path. Clients that don't set the matching client capability still
/// work; they just see the standalone fallbacks.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub supports_configuration_done_request: bool,
    pub supports_evaluate_for_hovers: bool,
    pub supports_step_in_targets_request: bool,
    pub supports_set_variable: bool,
    pub supports_set_expression: bool,
    pub supports_conditional_breakpoints: bool,
    pub supports_hit_conditional_breakpoints: bool,
    pub supports_log_points: bool,
    pub supports_function_breakpoints: bool,
    pub supports_restart_frame: bool,
    pub supports_exception_breakpoint_filters: bool,
    pub supports_terminate_request: bool,
    pub supports_cancel_request: bool,
    pub supports_harn_host_call: bool,
    /// Custom capability: advertises the `burin/promptProvenance` and
    /// `burin/promptConsumers` reverse-response requests that power the
    /// IDE's prompt-template source-map highlighting. See burin-code
    /// issues #93 and #94.
    pub supports_burin_prompt_provenance: bool,
    pub exception_breakpoint_filters: Vec<ExceptionBreakpointFilter>,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            supports_configuration_done_request: true,
            supports_evaluate_for_hovers: true,
            supports_step_in_targets_request: false,
            supports_set_variable: true,
            supports_set_expression: true,
            supports_conditional_breakpoints: true,
            supports_hit_conditional_breakpoints: true,
            supports_log_points: true,
            supports_function_breakpoints: true,
            supports_restart_frame: true,
            supports_exception_breakpoint_filters: true,
            supports_terminate_request: true,
            supports_cancel_request: false,
            supports_harn_host_call: true,
            supports_burin_prompt_provenance: true,
            exception_breakpoint_filters: vec![ExceptionBreakpointFilter {
                filter: "all".to_string(),
                label: "All Exceptions".to_string(),
                default: false,
            }],
        }
    }
}

/// DAP ExceptionBreakpointFilter for the initialize response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExceptionBreakpointFilter {
    pub filter: String,
    pub label: String,
    pub default: bool,
}

/// DAP Breakpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Breakpoint {
    pub id: i64,
    pub verified: bool,
    pub line: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// Conditional expression evaluated via `vm.evaluate_in_frame` on
    /// every hit; when falsy the breakpoint skips.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// Hit-count expression (`N`, `>=N`, `>N`, `%N`) honoured on every
    /// raw hit before the condition is evaluated. Matches VS Code's
    /// syntax so users can paste expressions between IDEs.
    #[serde(skip_serializing_if = "Option::is_none", rename = "hitCondition")]
    pub hit_condition: Option<String>,
    /// Logpoint message template. When present the breakpoint fires a
    /// DAP `output` event with `{expr}` interpolations resolved against
    /// the current frame and does NOT stop execution. Matches VS Code.
    #[serde(skip_serializing_if = "Option::is_none", rename = "logMessage")]
    pub log_message: Option<String>,
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
