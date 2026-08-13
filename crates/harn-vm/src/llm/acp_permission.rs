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

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

use crate::orchestration::{
    ToolPermissionDecider, ToolPermissionGrantScope, ToolPermissionOutcome,
    ToolPermissionPolicyEvidence, ToolPermissionResolution,
};
use crate::workspace_path::is_absolute_path_syntax;

/// Canonical ACP method for asking the client to decide a tool permission.
pub(crate) const METHOD_REQUEST_PERMISSION: &str = "session/request_permission";
/// Stable `optionId` for the canonical "allow this call" option. The
/// agent maps a `selected` response on this id to a grant.
pub(crate) const OPTION_ALLOW: &str = "allow";
/// Stable `optionId` for the canonical "reject this call" option.
pub(crate) const OPTION_REJECT: &str = "reject";

/// The canonical [`PermissionOption`]s the agent offers for a host-gated tool
/// call. Harn only offers semantics it can honor; clients may remember a
/// one-shot grant locally, but the runtime does not advertise persistence.
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
/// the canonical fields stay clean while harn-aware hosts (an IDE,
/// the REST surface) can still read them.
pub(crate) fn request_params(
    session_id: Option<&str>,
    tool_call_id: &str,
    tool_name: &str,
    raw_input: &JsonValue,
    approval_request: JsonValue,
    policy_decision: &JsonValue,
    tool_descriptor: Option<JsonValue>,
    tool_kind: crate::tool_annotations::ToolKind,
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
    let content = permission_content(&approval_request);
    let locations = permission_locations(&approval_request);
    let mut tool_call = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": tool_call_id,
        "title": tool_name,
        "kind": tool_kind,
        "rawInput": raw_input,
        "_meta": { "harn": harn_meta }
    });
    if !content.is_empty() {
        tool_call
            .as_object_mut()
            .expect("tool call object")
            .insert("content".to_string(), JsonValue::Array(content));
    }
    if !locations.is_empty() {
        tool_call
            .as_object_mut()
            .expect("tool call object")
            .insert("locations".to_string(), JsonValue::Array(locations));
    }
    params.insert("toolCall".to_string(), tool_call);
    params.insert("options".to_string(), canonical_options());
    JsonValue::Object(params)
}

fn permission_locations(approval_request: &JsonValue) -> Vec<JsonValue> {
    approval_request
        .get("evidence_refs")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter(|evidence| {
            evidence.get("kind").and_then(JsonValue::as_str) == Some("file_mutation_diff")
        })
        .filter_map(|evidence| evidence.get("path").and_then(JsonValue::as_str))
        .filter(|path| is_absolute_path_syntax(path))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|path| json!({ "path": path }))
        .collect()
}

fn permission_content(approval_request: &JsonValue) -> Vec<JsonValue> {
    approval_request
        .get("evidence_refs")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter(|evidence| evidence.get("kind").and_then(JsonValue::as_str) == Some("file_mutation_diff"))
        .filter_map(|evidence| {
            let path = evidence.get("path")?.as_str()?;
            let new_text = evidence.get("newText")?.as_str()?;
            let old_text = evidence.get("oldText").cloned().unwrap_or(JsonValue::Null);
            let preview_meta = json!({
                "source": evidence.get("source").cloned().unwrap_or_else(|| json!("pre_approval")),
                "preimageSha256": evidence.get("preimageSha256").cloned().unwrap_or(JsonValue::Null),
                "byteCount": evidence.get("byteCount").cloned().unwrap_or(JsonValue::Null),
            });
            Some(json!({
                "type": "diff",
                "path": path,
                "oldText": old_text,
                "newText": new_text,
                "_meta": {
                    "harn": {
                        "permission_preview": preview_meta
                    }
                }
            }))
        })
        .collect()
}

/// The agent's interpretation of a canonical permission response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WireOutcome {
    /// The selected canonical `allow_once` option.
    Allowed {
        resolution: ToolPermissionResolution,
    },
    /// `{ outcome: { outcome: "selected", optionId: "reject" } }` or a
    /// canonical cancellation. Both stop the tool call, while the typed
    /// resolution preserves denial, timeout, and cancellation without
    /// reclassifying human-facing prose.
    Rejected {
        reason: String,
        resolution: ToolPermissionResolution,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDecisionMetadata {
    schema: String,
    outcome: ToolPermissionOutcome,
    decider: ToolPermissionDecider,
    policy_evaluations: Vec<ToolPermissionPolicyEvidence>,
    #[serde(default)]
    grant_scope: Option<ToolPermissionGrantScope>,
}

fn decision_metadata(response: &JsonValue) -> Option<Result<WireDecisionMetadata, ()>> {
    response
        .pointer("/_meta/harn/permissionDecision")
        .map(|value| {
            serde_json::from_value(value.clone())
                .map_err(|_| ())
                .and_then(|metadata: WireDecisionMetadata| {
                    (metadata.schema == "harn.tool_permission_decision.v1")
                        .then_some(metadata)
                        .ok_or(())
                })
        })
}

fn approved_resolution(response: &JsonValue) -> Result<ToolPermissionResolution, ()> {
    let decision = match decision_metadata(response) {
        Some(decision) => decision?,
        None => WireDecisionMetadata {
            schema: "harn.tool_permission_decision.v1".to_string(),
            outcome: ToolPermissionOutcome::Approved,
            decider: ToolPermissionDecider::Person,
            policy_evaluations: Vec::new(),
            grant_scope: Some(ToolPermissionGrantScope::Once),
        },
    };
    if decision.outcome != ToolPermissionOutcome::Approved
        || decision.decider == ToolPermissionDecider::HostUnavailable
    {
        return Err(());
    }
    let scope = decision.grant_scope.ok_or(())?;
    ToolPermissionResolution::approved(decision.decider, scope)
        .with_host_policy_evaluations(decision.policy_evaluations)
        .map_err(|_| ())
}

fn terminal_resolution(
    response: &JsonValue,
    canonical_outcome: ToolPermissionOutcome,
) -> Result<ToolPermissionResolution, ()> {
    let decision = match decision_metadata(response) {
        Some(decision) => decision?,
        None => WireDecisionMetadata {
            schema: "harn.tool_permission_decision.v1".to_string(),
            outcome: canonical_outcome,
            decider: ToolPermissionDecider::Person,
            policy_evaluations: Vec::new(),
            grant_scope: None,
        },
    };
    let coherent = match canonical_outcome {
        ToolPermissionOutcome::Denied => decision.outcome == ToolPermissionOutcome::Denied,
        ToolPermissionOutcome::Cancelled => matches!(
            decision.outcome,
            ToolPermissionOutcome::Cancelled | ToolPermissionOutcome::TimedOut
        ),
        ToolPermissionOutcome::Approved | ToolPermissionOutcome::TimedOut => false,
    };
    if !coherent || decision.grant_scope.is_some() {
        return Err(());
    }
    ToolPermissionResolution::terminal(decision.outcome, decision.decider)
        .with_host_policy_evaluations(decision.policy_evaluations)
        .map_err(|_| ())
}

/// Parse a canonical `RequestPermissionResponse` result into a
/// [`WireOutcome`].
///
/// Canonical only: the response `result` is `{ outcome: <outcome> }` where
/// `<outcome>` is `{ outcome: "selected", optionId }` or
/// `{ outcome: "cancelled" }`. A `selected` outcome whose `optionId` is
/// not the offered allow option (including a missing id) is treated as a rejection
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
                match approved_resolution(response) {
                    Ok(resolution) => WireOutcome::Allowed { resolution },
                    Err(()) => WireOutcome::Rejected {
                        reason: "host returned invalid permission decision metadata".to_string(),
                        resolution: ToolPermissionResolution::terminal(
                            ToolPermissionOutcome::Denied,
                            ToolPermissionDecider::HostUnavailable,
                        ),
                    },
                }
            } else {
                WireOutcome::Rejected {
                    reason: response
                        .get("reason")
                        .and_then(JsonValue::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| "host rejected the tool call".to_string()),
                    resolution: terminal_resolution(response, ToolPermissionOutcome::Denied)
                        .unwrap_or_else(|()| {
                            ToolPermissionResolution::terminal(
                                ToolPermissionOutcome::Denied,
                                ToolPermissionDecider::HostUnavailable,
                            )
                        }),
                }
            }
        }
        "cancelled" => {
            let resolution = terminal_resolution(response, ToolPermissionOutcome::Cancelled)
                .unwrap_or_else(|()| {
                    ToolPermissionResolution::terminal(
                        ToolPermissionOutcome::Cancelled,
                        ToolPermissionDecider::HostUnavailable,
                    )
                });
            WireOutcome::Rejected {
                reason: match resolution.outcome {
                    ToolPermissionOutcome::TimedOut => "permission request timed out".to_string(),
                    _ => "permission request was cancelled".to_string(),
                },
                resolution,
            }
        }
        _ => WireOutcome::Rejected {
            reason: "host did not return a canonical permission outcome".to_string(),
            resolution: ToolPermissionResolution::terminal(
                ToolPermissionOutcome::Denied,
                ToolPermissionDecider::HostUnavailable,
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_annotations::ToolKind;

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
            ToolKind::Other,
        );
        assert_eq!(params["sessionId"], "session-1");
        assert_eq!(params["toolCall"]["sessionUpdate"], "tool_call_update");
        assert_eq!(params["toolCall"]["toolCallId"], "tool-1");
        assert_eq!(params["toolCall"]["title"], "edit");
        assert_eq!(params["toolCall"]["kind"], "other");
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
    fn request_params_project_file_evidence_into_canonical_diff_content() {
        let params = request_params(
            Some("session-1"),
            "tool-1",
            "edit",
            &json!({"path": "src/lib.rs"}),
            json!({
                "id": "tool-1",
                "action": "edit",
                "evidence_refs": [
                    {
                        "kind": "file_mutation_diff",
                        "path": "/workspace/src/lib.rs",
                        "oldText": "old\n",
                        "newText": "new\n",
                        "preimageSha256": "abc123",
                        "byteCount": 4,
                        "source": "pre_approval"
                    },
                    {
                        "kind": "file_mutation_diff",
                        "path": "/workspace/src/a.rs",
                        "newText": "new\n"
                    },
                    {
                        "kind": "file_mutation_diff",
                        "path": "/workspace/src/lib.rs",
                        "newText": "duplicate path\n"
                    },
                    {
                        "kind": "file_mutation_diff",
                        "path": r"C:\workspace\src\win.rs",
                        "newText": "windows path\n"
                    },
                    {
                        "kind": "command",
                        "path": "/workspace/ignored.rs"
                    },
                    {
                        "kind": "file_mutation_diff",
                        "path": "relative/not-valid-acp-location.rs",
                        "newText": "ignored\n"
                    }
                ]
            }),
            &json!({"decision": "ask"}),
            None,
            ToolKind::Edit,
        );

        let diff = &params["toolCall"]["content"][0];
        assert_eq!(diff["type"], "diff");
        assert_eq!(diff["path"], "/workspace/src/lib.rs");
        assert_eq!(diff["oldText"], "old\n");
        assert_eq!(diff["newText"], "new\n");
        assert_eq!(
            diff["_meta"]["harn"]["permission_preview"]["preimageSha256"],
            "abc123"
        );
        assert_eq!(
            params["toolCall"]["locations"],
            json!([
                { "path": "/workspace/src/a.rs" },
                { "path": "/workspace/src/lib.rs" },
                { "path": r"C:\workspace\src\win.rs" }
            ]),
            "file scopes are canonical, deterministic, and deduplicated"
        );
    }

    #[test]
    fn request_params_omit_locations_without_file_evidence() {
        let params = request_params(
            Some("session-1"),
            "tool-1",
            "run",
            &json!({"command": "pwd"}),
            json!({"id": "tool-1", "evidence_refs": []}),
            &json!({"decision": "ask"}),
            None,
            ToolKind::Execute,
        );

        assert!(params["toolCall"].get("locations").is_none());
    }

    #[test]
    fn request_params_use_the_declared_acp_tool_kind() {
        let params = request_params(
            Some("session-1"),
            "tool-1",
            "edit",
            &json!({"path": "src/lib.rs"}),
            json!({"id": "tool-1", "evidence_refs": []}),
            &json!({"decision": "ask"}),
            None,
            ToolKind::Edit,
        );

        assert_eq!(params["toolCall"]["kind"], "edit");
    }

    #[test]
    fn selected_allow_is_allowed() {
        let response = allow_response();
        assert!(matches!(
            parse_response(&response),
            WireOutcome::Allowed {
                resolution: ToolPermissionResolution {
                    outcome: ToolPermissionOutcome::Approved,
                    decider: ToolPermissionDecider::Person,
                    grant_scope: Some(ToolPermissionGrantScope::Once),
                    policy_evaluations,
                }
            } if policy_evaluations.is_empty()
        ));
    }

    #[test]
    fn every_offered_option_has_a_defined_parser_outcome() {
        for option in canonical_options().as_array().expect("options") {
            let option_id = option["optionId"].as_str().expect("option id");
            let kind = option["kind"].as_str().expect("option kind");
            let response = json!({
                "outcome": { "outcome": "selected", "optionId": option_id }
            });
            assert_eq!(
                matches!(parse_response(&response), WireOutcome::Allowed { .. }),
                kind.starts_with("allow_"),
                "offered option {option_id} ({kind})"
            );
        }
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
            WireOutcome::Rejected { reason, resolution } => {
                assert!(reason.contains("cancelled"));
                assert_eq!(resolution.outcome, ToolPermissionOutcome::Cancelled);
            }
            other => panic!("expected rejection, got {other:?}"),
        }
    }

    #[test]
    fn typed_host_metadata_preserves_remembered_and_timeout_deciders() {
        let approved = json!({
            "outcome": {"outcome": "selected", "optionId": OPTION_ALLOW},
            "_meta": {"harn": {"permissionDecision": {
                "schema": "harn.tool_permission_decision.v1",
                "outcome": "approved",
                "decider": "remembered_rule",
                "policy_evaluations": [{
                    "layer": "remembered_rule",
                    "outcome": "allowed",
                    "rule_id": "remember-travel",
                    "risk_labels": []
                }],
                "grant_scope": "session"
            }}}
        });
        let timed_out = json!({
            "outcome": {"outcome": "cancelled"},
            "_meta": {"harn": {"permissionDecision": {
                "schema": "harn.tool_permission_decision.v1",
                "outcome": "timed_out",
                "decider": "person",
                "policy_evaluations": [{
                    "layer": "managed_policy",
                    "outcome": "approval_required",
                    "risk_labels": ["external_mutation"]
                }]
            }}}
        });

        assert!(matches!(
            parse_response(&approved),
            WireOutcome::Allowed {
                resolution: ToolPermissionResolution {
                    decider: ToolPermissionDecider::RememberedRule,
                    grant_scope: Some(ToolPermissionGrantScope::Session),
                    ..
                }
            }
        ));
        assert!(matches!(
            parse_response(&timed_out),
            WireOutcome::Rejected {
                resolution: ToolPermissionResolution {
                    outcome: ToolPermissionOutcome::TimedOut,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn incoherent_or_reusable_host_metadata_fails_closed() {
        for metadata in [
            json!({
                "schema": "harn.tool_permission_decision.v1",
                "outcome": "denied",
                "decider": "person",
                "policy_evaluations": []
            }),
            json!({
                "schema": "harn.tool_permission_decision.v1",
                "outcome": "approved",
                "decider": "person",
                "policy_evaluations": [],
                "grant_scope": "once",
                "reusable": true
            }),
        ] {
            let response = json!({
                "outcome": {"outcome": "selected", "optionId": OPTION_ALLOW},
                "_meta": {"harn": {"permissionDecision": metadata}}
            });
            assert!(matches!(
                parse_response(&response),
                WireOutcome::Rejected {
                    resolution: ToolPermissionResolution {
                        decider: ToolPermissionDecider::HostUnavailable,
                        ..
                    },
                    ..
                }
            ));
        }
    }

    #[test]
    fn malformed_or_fabricated_host_policy_evidence_fails_closed() {
        for policy_evaluations in [
            json!([{
                "layer": "runtime_policy",
                "outcome": "allowed",
                "risk_labels": []
            }]),
            json!([{
                "layer": "managed_policy",
                "outcome": "allowed",
                "rule_id": "unsafe rule text",
                "risk_labels": []
            }]),
            json!([
                {"layer": "user_policy", "outcome": "allowed", "risk_labels": []},
                {"layer": "user_policy", "outcome": "allowed", "risk_labels": []}
            ]),
        ] {
            let response = json!({
                "outcome": {"outcome": "selected", "optionId": OPTION_ALLOW},
                "_meta": {"harn": {"permissionDecision": {
                    "schema": "harn.tool_permission_decision.v1",
                    "outcome": "approved",
                    "decider": "person",
                    "policy_evaluations": policy_evaluations,
                    "grant_scope": "once"
                }}}
            });
            assert!(matches!(
                parse_response(&response),
                WireOutcome::Rejected {
                    resolution: ToolPermissionResolution {
                        decider: ToolPermissionDecider::HostUnavailable,
                        ..
                    },
                    ..
                }
            ));
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
