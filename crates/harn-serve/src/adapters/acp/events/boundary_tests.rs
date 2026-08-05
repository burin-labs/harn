//! `boundary_failure` on the ACP wire (harn#5142).
//!
//! Pins the `_harn/agentEvent` payload shape so a field rename or a dropped key
//! fails here rather than silently breaking a host that renders the event.

use harn_vm::agent_events::AgentEvent;

use super::tests::collect_notifications;
use crate::adapters::acp::schema::HARN_AGENT_EVENT_METHOD;
use crate::adapters::acp::HARN_AGENT_EVENT_KINDS;

/// The loud-boundary funnel (harn#5142) on the wire. A client that renders
/// this can tell "the model produced nothing" from "the harness dropped what
/// the model produced" — the distinction the whole class of bug turned on.
#[tokio::test(flavor = "current_thread")]
async fn boundary_failure_reaches_acp_as_ext_event() {
    let actual = collect_notifications(vec![AgentEvent::BoundaryFailure {
        session_id: "session-1".to_string(),
        boundary: harn_vm::boundary::BoundaryId::TextToolParse,
        kind: harn_vm::boundary::BoundaryFailureKind::Unrecognized,
        owner: "harness".to_string(),
        detail: "the text tool-call parse boundary lost 1 span(s)".to_string(),
        excerpt: Some("<tool>read_file({ path: \"a.rs\" })</tool>".to_string()),
        dropped_count: 1,
        dropped_bytes: 38,
        unreported: false,
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "boundary_failure");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["boundary"], "text_tool_parse");
    assert_eq!(params["owner"], "harness");
    assert_eq!(params["droppedCount"], 1);
    assert_eq!(params["droppedBytes"], 38);
    assert_eq!(params["unreported"], false);
    assert!(params["excerpt"]
        .as_str()
        .is_some_and(|text| text.contains("<tool>")));
    assert!(
        HARN_AGENT_EVENT_KINDS.contains(&"boundary_failure"),
        "boundary_failure must be advertised so clients can subscribe"
    );
}

/// An excerpt is optional — a cap kill has no bytes to carry — and its absence
/// must not fabricate an empty string on the wire.
#[tokio::test(flavor = "current_thread")]
async fn boundary_failure_omits_an_absent_excerpt() {
    let actual = collect_notifications(vec![AgentEvent::BoundaryFailure {
        session_id: "session-1".to_string(),
        boundary: harn_vm::boundary::BoundaryId::ProviderAdmissionGate,
        kind: harn_vm::boundary::BoundaryFailureKind::Capped,
        owner: "policy".to_string(),
        detail: "the governor gave up".to_string(),
        excerpt: None,
        dropped_count: 0,
        dropped_bytes: 0,
        unreported: false,
    }])
    .await;

    let params = &actual[0]["params"];
    assert_eq!(params["kind"], "boundary_failure");
    assert_eq!(params["boundary"], "provider_admission_gate");
    assert_eq!(params["owner"], "policy");
    assert!(params["excerpt"].is_null());
}
