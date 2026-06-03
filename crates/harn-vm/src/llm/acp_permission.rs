//! Canonical ACP `session/request_permission` wire helpers.
//!
//! ACP v0.12.2 makes the request/response shapes for
//! `session/request_permission` canonical:
//!
//! - Request (agent -> client):
//!   `{ sessionId, toolCall: <ToolCallUpdate>, options: [{ optionId, name, kind }] }`
//! - Response (client -> agent):
//!   `{ outcome: { outcome: "selected", optionId } }` or
//!   `{ outcome: { outcome: "cancelled" } }`.
//!
//! There is no `{ outcome: "approved" }` / `{ granted }` in canonical ACP.
//!
//! This module owns *only* the canonical wire vocabulary. Harn's internal
//! permission *policy* decision (allow / deny / suspend), the
//! `ApprovalPolicy` receipt, and the out-of-band `harn.hitl.respond` HITL
//! path are deliberately untouched — they are harn semantics carried as
//! vendor extensions alongside the canonical fields.

use serde_json::{json, Value as JsonValue};

/// Stable `optionId` for the canonical "allow this call" option. The
/// agent maps a `selected` response on this id to a grant.
pub(crate) const OPTION_ALLOW: &str = "allow";
/// Stable `optionId` for the canonical "reject this call" option.
pub(crate) const OPTION_REJECT: &str = "reject";

/// The two canonical [`PermissionOption`]s the agent offers for a
/// host-gated tool call: allow-once and reject-once. The client renders
/// these and answers with `{ outcome: { outcome: "selected", optionId } }`.
fn canonical_options() -> JsonValue {
    json!([
        { "optionId": OPTION_ALLOW, "name": "Allow", "kind": "allow_once" },
        { "optionId": OPTION_REJECT, "name": "Reject", "kind": "reject_once" },
    ])
}

/// Canonical ACP `RequestPermissionResponse` granting the call.
pub(crate) fn allow_response() -> JsonValue {
    json!({
        "outcome": { "outcome": "selected", "optionId": OPTION_ALLOW }
    })
}

/// Canonical ACP `RequestPermissionResponse` rejecting the call.
pub(crate) fn reject_response(reason: Option<String>) -> JsonValue {
    let mut response = json!({
        "outcome": { "outcome": "selected", "optionId": OPTION_REJECT }
    });
    if let Some(reason) = reason {
        response
            .as_object_mut()
            .expect("object")
            .insert("reason".to_string(), JsonValue::String(reason));
    }
    response
}

/// Build the canonical `session/request_permission` request params.
///
/// `tool_call` is rooted as a canonical ACP `ToolCallUpdate`
/// (`{ sessionUpdate, toolCallId, title, kind, rawInput }`). Harn's
/// vendor extensions — the HITL `approvalRequest` envelope and the
/// `policyDecision` receipt — ride along under `toolCall._meta.harn` so
/// the canonical fields stay clean while harn-aware hosts (the Burin IDE,
/// the REST surface) can still read them.
pub(crate) fn request_params(
    session_id: Option<&str>,
    tool_call_id: &str,
    tool_name: &str,
    raw_input: &JsonValue,
    approval_request: JsonValue,
    policy_decision: &JsonValue,
    tool_descriptor: Option<JsonValue>,
) -> JsonValue {
    let mut params = serde_json::Map::new();
    if let Some(session_id) = session_id {
        params.insert("sessionId".to_string(), json!(session_id));
    }
    // The full tool descriptor (description + inputSchema, plus the rug-pull
    // `schemaChanged` flag) rides along so the host renders the *complete*
    // model-visible tool text at approval time, closing the tool-poisoning
    // visibility gap. Omitted when the catalog has no entry for the tool.
    let mut harn_meta = json!({
        "toolName": tool_name,
        "approvalRequest": approval_request,
        "policyDecision": policy_decision,
    });
    if let (Some(descriptor), Some(obj)) = (tool_descriptor, harn_meta.as_object_mut()) {
        obj.insert("toolDescriptor".to_string(), descriptor);
    }
    params.insert(
        "toolCall".to_string(),
        json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": tool_call_id,
            "title": tool_name,
            "kind": "other",
            "rawInput": raw_input,
            "_meta": { "harn": harn_meta }
        }),
    );
    params.insert("options".to_string(), canonical_options());
    JsonValue::Object(params)
}

/// The agent's interpretation of a canonical permission response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WireOutcome {
    /// `{ outcome: { outcome: "selected", optionId: "allow" } }`.
    Allowed,
    /// `{ outcome: { outcome: "selected", optionId: "reject" } }` or a
    /// `cancelled` outcome (the client dismissed the prompt). Both stop
    /// the tool call; `cancelled` is distinguished only by the default
    /// reason.
    Rejected { reason: String },
}

/// Parse a canonical `RequestPermissionResponse` result into a
/// [`WireOutcome`].
///
/// Canonical only: the response `result` is `{ outcome: <outcome> }` where
/// `<outcome>` is `{ outcome: "selected", optionId }` or
/// `{ outcome: "cancelled" }`. A `selected` outcome whose `optionId` is
/// not the allow option (including a missing id) is treated as a rejection
/// — fail closed.
pub(crate) fn parse_response(response: &JsonValue) -> WireOutcome {
    let outcome = response.get("outcome");
    let kind = outcome
        .and_then(|outcome| outcome.get("outcome"))
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    match kind {
        "selected" => {
            let option_id = outcome
                .and_then(|outcome| outcome.get("optionId"))
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            if option_id == OPTION_ALLOW {
                WireOutcome::Allowed
            } else {
                WireOutcome::Rejected {
                    reason: response
                        .get("reason")
                        .and_then(JsonValue::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| "host rejected the tool call".to_string()),
                }
            }
        }
        "cancelled" => WireOutcome::Rejected {
            reason: "permission request was cancelled".to_string(),
        },
        _ => WireOutcome::Rejected {
            reason: "host did not return a canonical permission outcome".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_params_carry_canonical_options_and_tool_call() {
        let params = request_params(
            Some("session-1"),
            "tool-1",
            "edit",
            &json!({"path": "src/lib.rs"}),
            json!({"id": "tool-1", "action": "edit"}),
            &json!({"decision": "ask"}),
            None,
        );
        assert_eq!(params["sessionId"], "session-1");
        assert_eq!(params["toolCall"]["sessionUpdate"], "tool_call_update");
        assert_eq!(params["toolCall"]["toolCallId"], "tool-1");
        assert_eq!(params["toolCall"]["title"], "edit");
        assert_eq!(params["toolCall"]["rawInput"]["path"], "src/lib.rs");
        assert_eq!(params["toolCall"]["_meta"]["harn"]["toolName"], "edit");
        assert_eq!(
            params["toolCall"]["_meta"]["harn"]["policyDecision"]["decision"],
            "ask"
        );
        let options = params["options"].as_array().expect("options array");
        assert_eq!(options.len(), 2);
        assert_eq!(options[0]["optionId"], OPTION_ALLOW);
        assert_eq!(options[0]["kind"], "allow_once");
        assert_eq!(options[1]["optionId"], OPTION_REJECT);
        assert_eq!(options[1]["kind"], "reject_once");
    }

    #[test]
    fn selected_allow_is_allowed() {
        let response = allow_response();
        assert_eq!(parse_response(&response), WireOutcome::Allowed);
    }

    #[test]
    fn selected_reject_is_rejected() {
        let response = reject_response(None);
        assert!(matches!(
            parse_response(&response),
            WireOutcome::Rejected { .. }
        ));
    }

    #[test]
    fn cancelled_is_rejected() {
        let response = json!({"outcome": {"outcome": "cancelled"}});
        match parse_response(&response) {
            WireOutcome::Rejected { reason } => assert!(reason.contains("cancelled")),
            other => panic!("expected rejection, got {other:?}"),
        }
    }

    #[test]
    fn non_canonical_outcome_fails_closed() {
        assert!(matches!(
            parse_response(&json!({"outcome": "approved"})),
            WireOutcome::Rejected { .. }
        ));
        assert!(matches!(
            parse_response(&json!({"granted": true})),
            WireOutcome::Rejected { .. }
        ));
    }

    #[test]
    fn selected_missing_option_id_fails_closed() {
        let response = json!({"outcome": {"outcome": "selected"}});
        assert!(matches!(
            parse_response(&response),
            WireOutcome::Rejected { .. }
        ));
    }
}
