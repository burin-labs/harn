//! Settling a pipeline: what is still outstanding when it ends.
//!
//! The unsettled snapshot starts empty, gains partial handoffs until they are
//! acknowledged, and — in its async form — also reports queued worker triggers
//! and uncancelled trigger-inbox events. Lifecycle audits get a monotonic
//! sequence and persist to the active event log; finalizing records a
//! disposition and clearing resets both audit and handoff state.

use crate::event_log::EventLog;
use crate::orchestration::*;
use std::collections::BTreeMap;
#[test]
fn unsettled_state_snapshot_starts_empty() {
    clear_pipeline_on_finish();
    let snapshot = unsettled_state_snapshot();
    assert!(snapshot.is_empty());
    assert_eq!(
        snapshot.to_json()["suspended_subagents"],
        serde_json::json!([])
    );
    assert_eq!(snapshot.to_json()["queued_triggers"], serde_json::json!([]));
    assert_eq!(
        snapshot.to_json()["partial_handoffs"],
        serde_json::json!([])
    );
    assert_eq!(
        snapshot.to_json()["in_flight_llm_calls"],
        serde_json::json!([])
    );
    assert_eq!(
        snapshot.to_json()["pool_pending_tasks"],
        serde_json::json!([])
    );
}

#[test]
fn record_lifecycle_audit_assigns_monotonic_seq() {
    clear_pipeline_on_finish();
    let a = record_lifecycle_audit("first", serde_json::json!({"x": 1}));
    let b = record_lifecycle_audit("second", serde_json::json!({"x": 2}));
    assert!(
        b.seq > a.seq,
        "seq must be monotonic ({} < {})",
        a.seq,
        b.seq
    );
    assert_eq!(a.kind, "first");
    assert_eq!(b.kind, "second");

    let drained = take_lifecycle_audit_log();
    assert_eq!(drained.len(), 2);
    assert!(
        take_lifecycle_audit_log().is_empty(),
        "log drains exactly once"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn record_lifecycle_audit_persists_to_active_event_log() {
    crate::event_log::reset_active_event_log();
    let log = crate::event_log::install_memory_for_current_thread(32);
    clear_pipeline_on_finish();

    let entry = record_lifecycle_audit("first", serde_json::json!({"x": 1}));
    let topic = crate::event_log::Topic::new(LIFECYCLE_AUDIT_TOPIC).unwrap();
    let events = log.read_range(&topic, None, usize::MAX).await.unwrap();

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1.kind, "lifecycle_audit");
    assert_eq!(events[0].1.payload["seq"], serde_json::json!(entry.seq));
    assert_eq!(events[0].1.payload["kind"], "first");

    crate::event_log::reset_active_event_log();
}

#[test]
fn record_partial_handoff_appears_in_unsettled_snapshot() {
    clear_pipeline_on_finish();
    let envelope = record_partial_handoff("downstream", serde_json::json!({"note": "x"}));
    assert!(envelope.envelope_id.starts_with("envelope_"));
    assert_eq!(envelope.target_pipeline, "downstream");

    let snapshot = unsettled_state_snapshot();
    assert!(!snapshot.is_empty());
    assert_eq!(snapshot.partial_handoffs.len(), 1);
    assert_eq!(
        snapshot.partial_handoffs[0]["target_pipeline"],
        "downstream"
    );
}

#[test]
fn acknowledge_partial_handoff_removes_envelope_and_audits() {
    clear_pipeline_on_finish();
    let envelope = record_partial_handoff("downstream", serde_json::json!({"note": "x"}));

    let removed = acknowledge_partial_handoff(
        &envelope.envelope_id,
        serde_json::json!({"decision": "accepted"}),
    )
    .expect("handoff should be acknowledged");

    assert_eq!(removed.envelope_id, envelope.envelope_id);
    assert!(unsettled_state_snapshot().partial_handoffs.is_empty());
    assert_eq!(
        lifecycle_audit_log_snapshot()
            .last()
            .map(|entry| entry.kind.as_str()),
        Some("handoff_acknowledged")
    );
}

#[test]
fn finalize_pipeline_records_disposition() {
    clear_pipeline_on_finish();
    let receipt = finalize_pipeline_disposition(serde_json::json!({"status": "completed"}));

    assert_eq!(receipt["status"], "finalized");
    assert_eq!(
        pipeline_disposition_snapshot(),
        Some(serde_json::json!({"status": "completed"}))
    );
    assert_eq!(
        lifecycle_audit_log_snapshot()
            .last()
            .map(|entry| entry.kind.as_str()),
        Some("pipeline_finalized")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn unsettled_state_snapshot_async_includes_worker_queue_triggers() {
    crate::event_log::reset_active_event_log();
    let log = crate::event_log::install_memory_for_current_thread(64);
    clear_pipeline_on_finish();
    let queue = crate::triggers::WorkerQueue::new(log);
    let job = crate::triggers::WorkerQueueJob {
        queue: "triage".to_string(),
        trigger_id: "incoming-review-task".to_string(),
        binding_key: "incoming-review-task@v1".to_string(),
        binding_version: 1,
        event: crate::triggers::TriggerEvent {
            id: crate::triggers::TriggerEventId("evt-1".to_string()),
            provider: crate::triggers::ProviderId::from("github"),
            kind: "issues.opened".to_string(),
            trace_id: crate::triggers::TraceId("trace-test".to_string()),
            dedupe_key: "evt-1".to_string(),
            tenant_id: None,
            headers: BTreeMap::new(),
            batch: None,
            raw_body: None,
            provider_payload: crate::triggers::ProviderPayload::Known(
                crate::triggers::event::KnownProviderPayload::Webhook(
                    crate::triggers::GenericWebhookPayload {
                        source: Some("lifecycle-test".to_string()),
                        content_type: Some("application/json".to_string()),
                        raw: serde_json::json!({"id": "evt-1"}),
                    },
                ),
            ),
            signature_status: crate::triggers::SignatureStatus::Verified,
            received_at: time::OffsetDateTime::now_utc(),
            occurred_at: None,
            dedupe_claimed: false,
        },
        replay_of_event_id: None,
        priority: crate::triggers::WorkerQueuePriority::Normal,
    };
    let receipt = queue.enqueue(&job).await.unwrap();

    let snapshot = unsettled_state_snapshot_async().await;
    assert_eq!(snapshot.queued_triggers.len(), 1);
    assert_eq!(
        snapshot.queued_triggers[0]["id"],
        format!("worker://triage/{}", receipt.job_event_id)
    );
    assert_eq!(snapshot.queued_triggers[0]["source"], "worker_queue");

    queue
        .ack_job("triage", receipt.job_event_id, "pipeline_lifecycle")
        .await
        .unwrap();
    assert!(unsettled_state_snapshot_async()
        .await
        .queued_triggers
        .is_empty());
    crate::event_log::reset_active_event_log();
}

#[tokio::test(flavor = "current_thread")]
async fn unsettled_state_snapshot_async_includes_uncancelled_trigger_inbox_events() {
    crate::event_log::reset_active_event_log();
    let log = crate::event_log::install_memory_for_current_thread(64);
    clear_pipeline_on_finish();
    let topic = crate::event_log::Topic::new(crate::triggers::TRIGGER_INBOX_ENVELOPES_TOPIC)
        .expect("static trigger inbox topic");
    let mut headers = BTreeMap::new();
    headers.insert(
        "binding_key".to_string(),
        "incoming-review-task@v1".to_string(),
    );
    headers.insert("trigger_id".to_string(), "incoming-review-task".to_string());
    log.append(
        &topic,
        crate::event_log::LogEvent::new(
            "event_ingested",
            serde_json::json!({
                "trigger_id": "incoming-review-task",
                "binding_version": 1,
                "event": {
                    "id": "evt-inbox",
                    "provider": "github",
                    "kind": "issues.opened",
                },
            }),
        )
        .with_headers(headers),
    )
    .await
    .unwrap();

    let snapshot = unsettled_state_snapshot_async().await;
    assert_eq!(snapshot.queued_triggers.len(), 1);
    assert_eq!(
        snapshot.queued_triggers[0]["id"],
        "trigger://incoming-review-task@v1/evt-inbox"
    );
    assert_eq!(snapshot.queued_triggers[0]["source"], "trigger_inbox");

    crate::triggers::append_dispatch_cancel_request(
        &log,
        &crate::triggers::DispatchCancelRequest {
            binding_key: "incoming-review-task@v1".to_string(),
            event_id: "evt-inbox".to_string(),
            requested_at: time::OffsetDateTime::now_utc(),
            requested_by: Some("test".to_string()),
            audit_id: None,
        },
    )
    .await
    .unwrap();
    assert!(unsettled_state_snapshot_async()
        .await
        .queued_triggers
        .is_empty());
    crate::event_log::reset_active_event_log();
}

#[test]
fn clear_pipeline_on_finish_resets_audit_and_handoff_state() {
    clear_pipeline_on_finish();
    record_lifecycle_audit("kind", serde_json::Value::Null);
    record_partial_handoff("downstream", serde_json::Value::Null);
    assert!(!lifecycle_audit_log_snapshot().is_empty());
    assert!(!unsettled_state_snapshot().partial_handoffs.is_empty());

    clear_pipeline_on_finish();
    assert!(lifecycle_audit_log_snapshot().is_empty());
    assert!(unsettled_state_snapshot().partial_handoffs.is_empty());
}
