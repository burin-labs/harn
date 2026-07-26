//! Deriving the observability view of a run.
//!
//! Trigger and predicate nodes share one trace id, a replayed trigger run gets a
//! replay chain back to the original, and compaction events are collected.
//! Action-graph updates publish to the event log with the payload redacted.

use crate::event_log::EventLog;
use crate::orchestration::*;
use futures::StreamExt;
use std::collections::BTreeMap;
#[test]
fn derive_run_observability_adds_trigger_and_predicate_nodes_with_shared_trace_id() {
    let trigger_event = crate::triggers::TriggerEvent {
        id: crate::triggers::TriggerEventId("trigger_evt_1".to_string()),
        provider: crate::triggers::ProviderId("cron".to_string()),
        kind: "tick".to_string(),
        received_at: time::OffsetDateTime::from_unix_timestamp(1_710_000_000).unwrap(),
        occurred_at: None,
        dedupe_key: "cron:daily".to_string(),
        trace_id: crate::triggers::TraceId("trace_123".to_string()),
        tenant_id: None,
        headers: BTreeMap::new(),
        raw_body: None,
        provider_payload: crate::triggers::ProviderPayload::Known(
            crate::triggers::event::KnownProviderPayload::Cron(crate::triggers::CronEventPayload {
                cron_id: Some("daily-review".to_string()),
                schedule: Some("0 9 * * 1-5".to_string()),
                tick_at: time::OffsetDateTime::from_unix_timestamp(1_710_000_000).unwrap(),
                raw: serde_json::json!({"scheduled": true}),
            }),
        ),
        signature_status: crate::triggers::SignatureStatus::Unsigned,
        dedupe_claimed: false,
        batch: None,
    };
    let run = RunRecord {
        id: "run_trigger_obs".to_string(),
        workflow_id: "wf".to_string(),
        workflow_name: Some("triggered workflow".to_string()),
        status: "completed".to_string(),
        stages: vec![
            RunStageRecord {
                id: "stage_gate".to_string(),
                node_id: "gate".to_string(),
                kind: "condition".to_string(),
                status: "completed".to_string(),
                outcome: "condition_true".to_string(),
                branch: Some("true".to_string()),
                ..Default::default()
            },
            RunStageRecord {
                id: "stage_act".to_string(),
                node_id: "act".to_string(),
                kind: "stage".to_string(),
                status: "completed".to_string(),
                outcome: "success".to_string(),
                ..Default::default()
            },
        ],
        transitions: vec![RunTransitionRecord {
            id: "transition_gate_act".to_string(),
            from_stage_id: Some("stage_gate".to_string()),
            from_node_id: Some("gate".to_string()),
            to_node_id: "act".to_string(),
            branch: Some("true".to_string()),
            timestamp: "transition".to_string(),
            consumed_artifact_ids: Vec::new(),
            produced_artifact_ids: Vec::new(),
        }],
        metadata: BTreeMap::from([(
            "trigger_event".to_string(),
            serde_json::to_value(&trigger_event).unwrap(),
        )]),
        ..Default::default()
    };

    let observability = derive_run_observability(&run, None);
    let trigger_node = observability
        .action_graph_nodes
        .iter()
        .find(|node| node.kind == "trigger")
        .expect("trigger node");
    let predicate_node = observability
        .action_graph_nodes
        .iter()
        .find(|node| node.kind == "predicate")
        .expect("predicate node");
    assert_eq!(trigger_node.trace_id.as_deref(), Some("trace_123"));
    assert_eq!(predicate_node.trace_id.as_deref(), Some("trace_123"));
    assert!(observability
        .action_graph_edges
        .iter()
        .any(|edge| edge.kind == "trigger_dispatch"));
    assert!(observability
        .action_graph_edges
        .iter()
        .any(|edge| edge.kind == "predicate_gate" && edge.label.as_deref() == Some("true")));
}

#[test]
fn derive_run_observability_adds_replay_chain_for_replayed_trigger_runs() {
    let trigger_event = crate::triggers::TriggerEvent {
        id: crate::triggers::TriggerEventId("trigger_evt_replay".to_string()),
        provider: crate::triggers::ProviderId("github".to_string()),
        kind: "issue.opened".to_string(),
        received_at: time::OffsetDateTime::from_unix_timestamp(1_710_000_000).unwrap(),
        occurred_at: None,
        dedupe_key: "github:replay".to_string(),
        trace_id: crate::triggers::TraceId("trace_replay".to_string()),
        tenant_id: None,
        headers: BTreeMap::new(),
        raw_body: None,
        provider_payload: crate::triggers::ProviderPayload::Known(
            crate::triggers::event::KnownProviderPayload::GitHub(Box::new(
                crate::triggers::GitHubEventPayload::Issues(
                    crate::triggers::event::GitHubIssuesEventPayload {
                        common: crate::triggers::event::GitHubEventCommon {
                            event: "issues".to_string(),
                            action: Some("opened".to_string()),
                            delivery_id: Some("delivery-replay".to_string()),
                            installation_id: Some(7),
                            topic: None,
                            reaction_topics: Vec::new(),
                            repository: None,
                            repo: None,
                            raw: serde_json::json!({"action":"opened"}),
                        },
                        issue: serde_json::json!({}),
                    },
                ),
            )),
        ),
        signature_status: crate::triggers::SignatureStatus::Verified,
        dedupe_claimed: false,
        batch: None,
    };
    let run = RunRecord {
        id: "run_replay_chain".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        metadata: BTreeMap::from([
            (
                "trigger_event".to_string(),
                serde_json::to_value(&trigger_event).unwrap(),
            ),
            (
                "replay_of_event_id".to_string(),
                serde_json::json!("trigger_evt_original"),
            ),
        ]),
        ..Default::default()
    };

    let observability = derive_run_observability(&run, None);
    assert!(observability.action_graph_nodes.iter().any(|node| {
        node.kind == "trigger" && node.label.contains("original trigger_evt_original")
    }));
    assert!(observability.action_graph_edges.iter().any(|edge| {
        edge.kind == "replay_chain" && edge.label.as_deref() == Some("replay chain")
    }));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn save_run_record_publishes_action_graph_updates_to_event_log() {
    crate::reset_thread_local_state();
    let temp_dir = tempfile::tempdir().unwrap();
    let run_path = temp_dir.path().join("run.json");
    crate::event_log::install_memory_for_current_thread(8);
    let topic = crate::event_log::Topic::new("observability.action_graph").unwrap();
    let log = crate::event_log::active_event_log().expect("active event log");
    let mut stream = log.clone().subscribe(&topic, None).await.unwrap();

    let mut run = RunRecord {
        id: "run_event_log".to_string(),
        workflow_id: "wf".to_string(),
        workflow_name: Some("event-log workflow".to_string()),
        status: "running".to_string(),
        stages: vec![RunStageRecord {
            id: "stage_gate".to_string(),
            node_id: "gate".to_string(),
            kind: "condition".to_string(),
            status: "completed".to_string(),
            outcome: "condition_true".to_string(),
            branch: Some("true".to_string()),
            ..Default::default()
        }],
        metadata: BTreeMap::from([(
            "trigger_event".to_string(),
            serde_json::json!({
                "id": "trigger_evt_stream",
                "provider": "cron",
                "kind": "tick",
                "received_at": "2026-04-19T16:00:00Z",
                "occurred_at": null,
                "dedupe_key": "cron:stream",
                "trace_id": "trace_stream",
                "tenant_id": null,
                "headers": {},
                "provider_payload": {
                    "provider": "cron",
                    "cron_id": "stream",
                    "schedule": "0 * * * *",
                    "tick_at": "2026-04-19T16:00:00Z",
                    "raw": {}
                },
                "signature_status": {"state": "unsigned"}
            }),
        )]),
        ..Default::default()
    };

    let _policy_guard = crate::redact::PolicyGuard::new(
        crate::redact::RedactionPolicy::default().with_extra_field("workflow_id"),
    );
    save_run_record(&run, Some(run_path.to_str().unwrap())).unwrap();
    run.status = "completed".to_string();
    save_run_record(&run, Some(run_path.to_str().unwrap())).unwrap();
    let events = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut events = Vec::new();
        while events.len() < 2 {
            let (_, event) = stream.next().await.unwrap().unwrap();
            if event.kind == "action_graph_update"
                && event.headers.get("run_id").map(String::as_str) == Some(run.id.as_str())
            {
                events.push(event);
            }
        }
        events
    })
    .await
    .expect("timed out waiting for this run's action graph events");
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .all(|event| event.kind == "action_graph_update"));
    assert!(events.iter().all(|event| {
        event.headers.get("trace_id").map(String::as_str) == Some("trace_stream")
    }));
    assert!(events
        .iter()
        .all(|event| event.payload["workflow_id"] == serde_json::json!("[redacted]")));
    assert!(events.iter().any(|event| {
        event.payload["observability"]["action_graph_nodes"]
            .as_array()
            .is_some_and(|nodes| {
                nodes.iter().any(|node| {
                    node.get("kind").and_then(|value| value.as_str()) == Some("trigger")
                })
            })
    }));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn action_graph_update_redacts_payload_before_append() {
    crate::reset_thread_local_state();
    let log = crate::event_log::install_memory_for_current_thread(8);
    let topic = crate::event_log::Topic::new("observability.action_graph").unwrap();

    append_action_graph_update(
        BTreeMap::from([(
            "Authorization".to_string(),
            "Bearer raw-action-header".to_string(),
        )]),
        serde_json::json!({
            "run_id": "run_action",
            "provider_payload": {
                "api_key": "raw-action-api-key",
                "callback": "https://user:password@example.com/cb?access_token=raw-action-token"
            }
        }),
    )
    .await
    .unwrap();

    let events = log.read_range(&topic, None, 8).await.unwrap();
    assert_eq!(events.len(), 1);
    let persisted = serde_json::to_string(&events[0].1).unwrap();
    assert!(persisted.contains("[redacted]") || persisted.contains("%5Bredacted%5D"));
    for secret in [
        "raw-action-header",
        "raw-action-api-key",
        "user:password",
        "raw-action-token",
    ] {
        assert!(
            !persisted.contains(secret),
            "action-graph event persisted secret {secret}: {persisted}"
        );
    }
    crate::event_log::reset_active_event_log();
}

#[test]
fn derive_run_observability_collects_compaction_events() {
    let transcript = serde_json::json!({
        "_type": "transcript",
        "id": "session-compaction",
        "messages": [
            {"role": "user", "content": "summary"}
        ],
        "events": [
            {
                "id": "compaction-event-1",
                "kind": "compaction",
                "role": "system",
                "visibility": "internal",
                "text": "Transcript compacted via truncate",
                "metadata": {
                    "mode": "manual",
                    "strategy": "truncate",
                    "archived_messages": 3,
                    "estimated_tokens_before": 120,
                    "estimated_tokens_after": 48,
                    "snapshot_asset_id": "snapshot-1"
                }
            }
        ],
        "assets": [
            {
                "id": "snapshot-1",
                "kind": "compaction_source_transcript",
                "visibility": "internal",
                "data": {
                    "_type": "transcript",
                    "id": "session-compaction",
                    "messages": [
                        {"role": "user", "content": "first"},
                        {"role": "assistant", "content": "second"},
                        {"role": "user", "content": "third"},
                        {"role": "assistant", "content": "fourth"}
                    ]
                }
            }
        ]
    });
    let run = RunRecord {
        id: "run_compaction".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        transcript: Some(transcript),
        ..Default::default()
    };

    let observability = derive_run_observability(&run, None);
    assert_eq!(observability.compaction_events.len(), 1);
    let event = &observability.compaction_events[0];
    assert_eq!(event.id, "compaction-event-1");
    assert_eq!(event.mode, "manual");
    assert_eq!(event.strategy, "truncate");
    assert_eq!(event.archived_messages, 3);
    assert_eq!(event.estimated_tokens_before, 120);
    assert_eq!(event.estimated_tokens_after, 48);
    assert_eq!(event.snapshot_asset_id.as_deref(), Some("snapshot-1"));
    assert_eq!(event.snapshot_location, "run.transcript.assets[snapshot-1]");
    assert!(event.available);
}
