//! Orchestration integration tests for policy/workflow/mutation-session.

use super::*;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::event_log::EventLog;

#[test]
fn capability_intersection_rejects_privilege_expansion() {
    let ceiling = CapabilityPolicy {
        tools: vec!["read".to_string()],
        side_effect_level: Some("read_only".to_string()),
        recursion_limit: Some(2),
        ..Default::default()
    };
    let requested = CapabilityPolicy {
        tools: vec!["read".to_string(), "edit".to_string()],
        ..Default::default()
    };
    let error = ceiling.intersect(&requested).unwrap_err();
    assert!(error.contains("host ceiling"));
}

#[test]
fn mutation_session_normalize_fills_defaults() {
    let normalized = MutationSessionRecord::default().normalize();
    assert!(normalized.session_id.starts_with("session_"));
    assert_eq!(normalized.mutation_scope, "read_only");
    assert!(normalized.approval_policy.is_none());
}

#[test]
fn install_current_mutation_session_round_trips() {
    let policy = ToolApprovalPolicy {
        require_approval: vec!["edit*".to_string()],
        ..Default::default()
    };
    install_current_mutation_session(Some(MutationSessionRecord {
        session_id: "session_test".to_string(),
        mutation_scope: "apply_workspace".to_string(),
        approval_policy: Some(policy.clone()),
        ..Default::default()
    }));
    let current = current_mutation_session().expect("session installed");
    assert_eq!(current.session_id, "session_test");
    assert_eq!(current.mutation_scope, "apply_workspace");
    assert_eq!(current.approval_policy.as_ref(), Some(&policy));

    install_current_mutation_session(None);
    assert!(current_mutation_session().is_none());
}

#[test]
fn active_execution_policy_rejects_unknown_bridge_builtin() {
    push_execution_policy(CapabilityPolicy {
        tools: vec!["read".to_string()],
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        side_effect_level: Some("read_only".to_string()),
        recursion_limit: Some(1),
        ..Default::default()
    });
    let error = enforce_current_policy_for_bridge_builtin("custom_host_builtin").unwrap_err();
    pop_execution_policy();
    assert!(matches!(
        error,
        VmError::CategorizedError {
            category: crate::value::ErrorCategory::ToolRejected,
            ..
        }
    ));
}

#[test]
fn active_execution_policy_rejects_mcp_escape_hatch() {
    push_execution_policy(CapabilityPolicy {
        tools: vec!["read".to_string()],
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        side_effect_level: Some("read_only".to_string()),
        recursion_limit: Some(1),
        ..Default::default()
    });
    let error = enforce_current_policy_for_builtin("mcp_connect", &[]).unwrap_err();
    pop_execution_policy();
    assert!(matches!(
        error,
        VmError::CategorizedError {
            category: crate::value::ErrorCategory::ToolRejected,
            ..
        }
    ));
}

#[test]
fn workflow_normalization_upgrades_legacy_act_verify_repair_shape() {
    let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "name": "legacy",
        "act": {"mode": "llm"},
        "verify": {"kind": "verify"},
        "repair": {"mode": "agent"},
    }));
    let graph = normalize_workflow_value(&value).unwrap();
    assert_eq!(graph.type_name, "workflow_graph");
    assert!(graph.nodes.contains_key("act"));
    assert!(graph.nodes.contains_key("verify"));
    assert!(graph.nodes.contains_key("repair"));
    assert_eq!(graph.entry, "act");
}

#[test]
fn workflow_normalization_accepts_tool_registry_nodes() {
    let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "name": "registry_tools",
        "entry": "implement",
        "nodes": {
            "implement": {
                "kind": "stage",
                "mode": "agent",
                "tools": {
                    "_type": "tool_registry",
                    "tools": [
                        {"name": "read", "description": "Read files"},
                        {"name": "run", "description": "Run commands"}
                    ]
                }
            }
        },
        "edges": []
    }));
    let graph = normalize_workflow_value(&value).unwrap();
    let node = graph.nodes.get("implement").unwrap();
    assert_eq!(workflow_tool_names(&node.tools), vec!["read", "run"]);
}

#[test]
fn artifact_selection_honors_budget_and_priority() {
    let policy = ContextPolicy {
        max_artifacts: Some(2),
        max_tokens: Some(30),
        prefer_recent: true,
        prefer_fresh: true,
        prioritize_kinds: vec!["verification_result".to_string()],
        ..Default::default()
    };
    let artifacts = vec![
        ArtifactRecord {
            type_name: "artifact".to_string(),
            id: "a".to_string(),
            kind: "summary".to_string(),
            text: Some("short".to_string()),
            relevance: Some(0.9),
            created_at: now_rfc3339(),
            ..Default::default()
        }
        .normalize(),
        ArtifactRecord {
            type_name: "artifact".to_string(),
            id: "b".to_string(),
            kind: "summary".to_string(),
            text: Some("this is a much larger artifact body".to_string()),
            relevance: Some(1.0),
            created_at: now_rfc3339(),
            ..Default::default()
        }
        .normalize(),
        ArtifactRecord {
            type_name: "artifact".to_string(),
            id: "c".to_string(),
            kind: "summary".to_string(),
            text: Some("tiny".to_string()),
            relevance: Some(0.5),
            created_at: now_rfc3339(),
            ..Default::default()
        }
        .normalize(),
    ];
    let selected = select_artifacts(artifacts, &policy);
    assert_eq!(selected.len(), 2);
    assert!(selected.iter().all(|artifact| artifact.kind == "summary"));
}

#[test]
fn workflow_validation_rejects_condition_without_true_false_edges() {
    let graph = WorkflowGraph {
        entry: "gate".to_string(),
        nodes: BTreeMap::from([(
            "gate".to_string(),
            WorkflowNode {
                id: Some("gate".to_string()),
                kind: "condition".to_string(),
                ..Default::default()
            },
        )]),
        edges: vec![WorkflowEdge {
            from: "gate".to_string(),
            to: "next".to_string(),
            branch: Some("true".to_string()),
            label: None,
        }],
        ..Default::default()
    };
    let report = validate_workflow(&graph, None);
    assert!(!report.valid);
    assert!(report
        .errors
        .iter()
        .any(|error| error.contains("true") && error.contains("false")));
}

#[test]
fn replay_fixture_round_trip_passes() {
    let run = RunRecord {
        type_name: "run_record".to_string(),
        id: "run_1".to_string(),
        workflow_id: "wf".to_string(),
        workflow_name: Some("demo".to_string()),
        task: "demo".to_string(),
        status: "completed".to_string(),
        started_at: "1".to_string(),
        finished_at: Some("2".to_string()),
        parent_run_id: None,
        root_run_id: Some("run_1".to_string()),
        stages: vec![RunStageRecord {
            id: "stage_1".to_string(),
            node_id: "act".to_string(),
            kind: "stage".to_string(),
            status: "completed".to_string(),
            outcome: "success".to_string(),
            branch: Some("success".to_string()),
            started_at: "1".to_string(),
            finished_at: Some("2".to_string()),
            visible_text: Some("done".to_string()),
            private_reasoning: None,
            transcript: None,
            verification: None,
            usage: None,
            artifacts: vec![ArtifactRecord {
                type_name: "artifact".to_string(),
                id: "a1".to_string(),
                kind: "summary".to_string(),
                text: Some("done".to_string()),
                created_at: "1".to_string(),
                ..Default::default()
            }
            .normalize()],
            consumed_artifact_ids: vec![],
            produced_artifact_ids: vec!["a1".to_string()],
            attempts: vec![],
            metadata: BTreeMap::new(),
        }],
        transitions: vec![],
        checkpoints: vec![],
        pending_nodes: vec![],
        completed_nodes: vec!["act".to_string()],
        child_runs: vec![],
        artifacts: vec![],
        policy: CapabilityPolicy::default(),
        execution: None,
        transcript: None,
        usage: None,
        replay_fixture: None,
        observability: None,
        trace_spans: vec![],
        tool_recordings: vec![],
        metadata: BTreeMap::new(),
        persisted_path: None,
    };
    let fixture = replay_fixture_from_run(&run);
    let report = evaluate_run_against_fixture(&run, &fixture);
    assert!(report.pass);
    assert!(report.failures.is_empty());
}

#[test]
fn replay_eval_suite_reports_failed_case() {
    let good = RunRecord {
        id: "run_good".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        stages: vec![RunStageRecord {
            node_id: "act".to_string(),
            status: "completed".to_string(),
            outcome: "success".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let bad = RunRecord {
        id: "run_bad".to_string(),
        workflow_id: "wf".to_string(),
        status: "failed".to_string(),
        stages: vec![RunStageRecord {
            node_id: "act".to_string(),
            status: "failed".to_string(),
            outcome: "error".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let suite = evaluate_run_suite(vec![
        (
            good.clone(),
            replay_fixture_from_run(&good),
            Some("good.json".to_string()),
        ),
        (
            bad.clone(),
            replay_fixture_from_run(&good),
            Some("bad.json".to_string()),
        ),
    ]);
    assert!(!suite.pass);
    assert_eq!(suite.total, 2);
    assert_eq!(suite.failed, 1);
    assert!(suite.cases.iter().any(|case| !case.pass));
}

#[test]
fn run_diff_reports_changed_stage() {
    let left = RunRecord {
        id: "left".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        stages: vec![RunStageRecord {
            node_id: "act".to_string(),
            status: "completed".to_string(),
            outcome: "success".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let right = RunRecord {
        id: "right".to_string(),
        workflow_id: "wf".to_string(),
        status: "failed".to_string(),
        stages: vec![RunStageRecord {
            node_id: "act".to_string(),
            status: "failed".to_string(),
            outcome: "error".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let diff = diff_run_records(&left, &right);
    assert!(diff.status_changed);
    assert!(!diff.identical);
    assert_eq!(diff.stage_diffs.len(), 1);
}

#[test]
fn save_and_load_run_record_materializes_observability_summary() {
    let temp_dir = tempfile::tempdir().unwrap();
    let run_path = temp_dir.path().join("run.json");
    let sidecar_dir = temp_dir.path().join("run-llm");
    std::fs::create_dir_all(&sidecar_dir).unwrap();
    std::fs::write(sidecar_dir.join("llm_transcript.jsonl"), "").unwrap();

    let run = RunRecord {
        id: "run_obs".to_string(),
        workflow_id: "wf".to_string(),
        workflow_name: Some("demo".to_string()),
        task: "debug a failing run".to_string(),
        status: "failed".to_string(),
        stages: vec![RunStageRecord {
            id: "stage_1".to_string(),
            node_id: "plan".to_string(),
            kind: "stage".to_string(),
            status: "failed".to_string(),
            outcome: "error".to_string(),
            verification: Some(serde_json::json!({"pass": false, "reason": "assertion failed"})),
            artifacts: vec![ArtifactRecord {
                data: Some(serde_json::json!({
                    "trace": {
                        "iterations": 3,
                        "llm_calls": 2,
                        "tool_executions": 1,
                        "tool_rejections": 0,
                        "interventions": 1,
                        "compactions": 0,
                        "tools_used": ["read"]
                    },
                    "tools_used": ["read"],
                    "successful_tools": ["read"],
                    "ledger_done_rejections": 1,
                    "task_ledger": {
                        "root_task": "debug a failing run",
                        "rationale": "explain the regression",
                        "deliverables": [
                            {"id": "deliverable-1", "text": "find the root cause", "status": "blocked", "note": "verification failed"}
                        ],
                        "observations": ["verify stage failed after read"]
                    }
                })),
                ..Default::default()
            }],
            ..Default::default()
        }],
        child_runs: vec![RunChildRecord {
            worker_id: "worker-1".to_string(),
            worker_name: "researcher".to_string(),
            parent_stage_id: Some("stage_1".to_string()),
            run_id: Some("child-run".to_string()),
            run_path: Some("child.json".to_string()),
            status: "completed".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };

    save_run_record(&run, Some(run_path.to_str().unwrap())).unwrap();
    let loaded = load_run_record(&run_path).unwrap();
    let observability = loaded.observability.expect("observability summary");
    assert_eq!(observability.schema_version, 4);
    assert_eq!(observability.planner_rounds.len(), 1);
    assert_eq!(observability.research_fact_count, 1);
    assert_eq!(observability.worker_lineage.len(), 1);
    assert_eq!(observability.verification_outcomes.len(), 1);
    assert!(observability.compaction_events.is_empty());
    assert!(observability
        .transcript_pointers
        .iter()
        .any(|pointer| pointer.kind == "llm_jsonl" && pointer.available));
    assert_eq!(
        observability.planner_rounds[0].research_facts,
        vec!["verify stage failed after read".to_string()]
    );
}

#[test]
fn save_and_load_run_record_materializes_daemon_events_from_sidecar() {
    let temp_dir = tempfile::tempdir().unwrap();
    let run_path = temp_dir.path().join("run.json");
    let sidecar_dir = temp_dir.path().join("run-llm");
    std::fs::create_dir_all(&sidecar_dir).unwrap();
    std::fs::write(
        sidecar_dir.join("llm_transcript.jsonl"),
        concat!(
            "{\"type\":\"daemon_event\",\"timestamp\":\"1710000000.100\",\"daemon_id\":\"daemon-1\",\"name\":\"reviewer\",\"kind\":\"spawned\",\"persist_path\":\"/tmp/reviewer\",\"payload_summary\":\"always-on reviewer\"}\n",
            "{\"type\":\"daemon_event\",\"timestamp\":\"1710000001.200\",\"daemon_id\":\"daemon-1\",\"name\":\"reviewer\",\"kind\":\"triggered\",\"persist_path\":\"/tmp/reviewer\",\"payload_summary\":\"new review requested\"}\n"
        ),
    )
    .unwrap();

    let run = RunRecord {
        id: "run_daemon_obs".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        ..Default::default()
    };

    save_run_record(&run, Some(run_path.to_str().unwrap())).unwrap();
    let loaded = load_run_record(&run_path).unwrap();
    let observability = loaded.observability.expect("observability summary");
    assert_eq!(observability.daemon_events.len(), 2);
    assert_eq!(observability.daemon_events[0].daemon_id, "daemon-1");
    assert_eq!(observability.daemon_events[0].name, "reviewer");
    assert_eq!(
        observability.daemon_events[0].kind,
        super::DaemonEventKindRecord::Spawned
    );
    assert_eq!(
        observability.daemon_events[1].payload_summary.as_deref(),
        Some("new review requested")
    );
}

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
        provider_payload: crate::triggers::ProviderPayload::Known(
            crate::triggers::event::KnownProviderPayload::Cron(crate::triggers::CronEventPayload {
                cron_id: Some("daily-review".to_string()),
                schedule: Some("0 9 * * 1-5".to_string()),
                tick_at: time::OffsetDateTime::from_unix_timestamp(1_710_000_000).unwrap(),
                raw: serde_json::json!({"scheduled": true}),
            }),
        ),
        signature_status: crate::triggers::SignatureStatus::Unsigned,
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
        provider_payload: crate::triggers::ProviderPayload::Known(
            crate::triggers::event::KnownProviderPayload::GitHub(
                crate::triggers::GitHubEventPayload::Issues(
                    crate::triggers::event::GitHubIssuesEventPayload {
                        common: crate::triggers::event::GitHubEventCommon {
                            event: "issues".to_string(),
                            action: Some("opened".to_string()),
                            delivery_id: Some("delivery-replay".to_string()),
                            installation_id: Some(7),
                            raw: serde_json::json!({"action":"opened"}),
                        },
                        issue: serde_json::json!({}),
                    },
                ),
            ),
        ),
        signature_status: crate::triggers::SignatureStatus::Verified,
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

#[tokio::test(flavor = "current_thread")]
async fn save_run_record_publishes_action_graph_updates_to_event_log() {
    crate::reset_thread_local_state();
    let temp_dir = tempfile::tempdir().unwrap();
    let run_path = temp_dir.path().join("run.json");
    crate::event_log::install_default_for_base_dir(temp_dir.path()).expect("install event log");

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

    save_run_record(&run, Some(run_path.to_str().unwrap())).unwrap();
    run.status = "completed".to_string();
    save_run_record(&run, Some(run_path.to_str().unwrap())).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(25)).await;

    let topic = crate::event_log::Topic::new("observability.action_graph").unwrap();
    let log = crate::event_log::active_event_log().expect("active event log");
    let events = log.read_range(&topic, None, usize::MAX).await.unwrap();
    assert_eq!(events.len(), 2);
    assert!(events
        .iter()
        .all(|(_, event)| event.kind == "action_graph_update"));
    assert!(events.iter().all(|(_, event)| {
        event.headers.get("trace_id").map(String::as_str) == Some("trace_stream")
    }));
    assert!(events.iter().any(|(_, event)| {
        event.payload["observability"]["action_graph_nodes"]
            .as_array()
            .is_some_and(|nodes| {
                nodes.iter().any(|node| {
                    node.get("kind").and_then(|value| value.as_str()) == Some("trigger")
                })
            })
    }));
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

#[test]
fn eval_suite_manifest_can_fail_on_baseline_diff() {
    let temp_dir = std::env::temp_dir().join(format!("harn-eval-suite-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let baseline_path = temp_dir.join("baseline.json");
    let candidate_path = temp_dir.join("candidate.json");

    let baseline = RunRecord {
        id: "baseline".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        stages: vec![RunStageRecord {
            node_id: "act".to_string(),
            status: "completed".to_string(),
            outcome: "success".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let candidate = RunRecord {
        id: "candidate".to_string(),
        workflow_id: "wf".to_string(),
        status: "failed".to_string(),
        stages: vec![RunStageRecord {
            node_id: "act".to_string(),
            status: "failed".to_string(),
            outcome: "error".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };

    save_run_record(&baseline, Some(baseline_path.to_str().unwrap())).unwrap();
    save_run_record(&candidate, Some(candidate_path.to_str().unwrap())).unwrap();

    let manifest = EvalSuiteManifest {
        base_dir: Some(temp_dir.display().to_string()),
        cases: vec![EvalSuiteCase {
            label: Some("candidate".to_string()),
            run_path: "candidate.json".to_string(),
            fixture_path: None,
            compare_to: Some("baseline.json".to_string()),
        }],
        ..Default::default()
    };
    let suite = evaluate_run_suite_manifest(&manifest).unwrap();
    assert!(!suite.pass);
    assert_eq!(suite.failed, 1);
    assert!(suite.cases[0].comparison.is_some());
    assert!(suite.cases[0]
        .failures
        .iter()
        .any(|failure| failure.contains("baseline")));
}

#[test]
fn render_unified_diff_marks_removed_and_added_lines() {
    let diff = render_unified_diff(Some("src/main.rs"), "old\nsame", "new\nsame");
    assert!(diff.contains("--- a/src/main.rs"));
    assert!(diff.contains("+++ b/src/main.rs"));
    assert!(diff.contains("-old"));
    assert!(diff.contains("+new"));
    assert!(diff.contains(" same"));
}

#[test]
fn render_unified_diff_identical_inputs() {
    let text = "line1\nline2\nline3";
    let diff = render_unified_diff(None, text, text);
    assert!(diff.contains("--- a/artifact"));
    let body: Vec<&str> = diff.lines().skip(2).collect();
    assert!(!body.iter().any(|l| l.starts_with('-')));
    assert!(!body.iter().any(|l| l.starts_with('+')));
    assert_eq!(body.len(), 3);
}

#[test]
fn render_unified_diff_empty_before() {
    let diff = render_unified_diff(None, "", "new1\nnew2");
    assert!(diff.contains("+new1"));
    assert!(diff.contains("+new2"));
    let body: Vec<&str> = diff.lines().skip(2).collect();
    assert!(!body.iter().any(|l| l.starts_with('-')));
}

#[test]
fn render_unified_diff_empty_after() {
    let diff = render_unified_diff(None, "old1\nold2", "");
    assert!(diff.contains("-old1"));
    assert!(diff.contains("-old2"));
    let body: Vec<&str> = diff.lines().skip(2).collect();
    assert!(!body.iter().any(|l| l.starts_with('+')));
}

#[test]
fn render_unified_diff_both_empty() {
    let diff = render_unified_diff(None, "", "");
    assert!(diff.contains("--- a/artifact"));
    assert!(diff.contains("+++ b/artifact"));
    let body: String = diff.lines().skip(2).collect();
    assert!(body.is_empty());
}

#[test]
fn render_unified_diff_all_changed() {
    let diff = render_unified_diff(None, "a\nb", "x\ny");
    assert!(diff.contains("-a"));
    assert!(diff.contains("-b"));
    assert!(diff.contains("+x"));
    assert!(diff.contains("+y"));
}

#[test]
fn render_unified_diff_insertion_in_middle() {
    let diff = render_unified_diff(None, "a\nc", "a\nb\nc");
    assert!(diff.contains(" a"));
    assert!(diff.contains("+b"));
    assert!(diff.contains(" c"));
    let body: Vec<&str> = diff.lines().skip(2).collect();
    assert!(!body.iter().any(|l| l.starts_with('-')));
}

#[test]
fn render_unified_diff_deletion_from_middle() {
    let diff = render_unified_diff(None, "a\nb\nc", "a\nc");
    assert!(diff.contains(" a"));
    assert!(diff.contains("-b"));
    assert!(diff.contains(" c"));
    let body: Vec<&str> = diff.lines().skip(2).collect();
    assert!(!body.iter().any(|l| l.starts_with('+')));
}

#[test]
fn render_unified_diff_default_path() {
    let diff = render_unified_diff(None, "a", "b");
    assert!(diff.contains("--- a/artifact"));
    assert!(diff.contains("+++ b/artifact"));
}

#[test]
fn render_unified_diff_large_similar() {
    let mut before = Vec::new();
    let mut after = Vec::new();
    for i in 0..1000 {
        before.push(format!("line {i}"));
        after.push(format!("line {i}"));
    }
    before[500] = "OLD LINE 500".to_string();
    after[500] = "NEW LINE 500".to_string();
    let before_str = before.join("\n");
    let after_str = after.join("\n");
    let diff = render_unified_diff(None, &before_str, &after_str);
    assert!(diff.contains("-OLD LINE 500"));
    assert!(diff.contains("+NEW LINE 500"));
    assert!(diff.contains(" line 499"));
    assert!(diff.contains(" line 501"));
}

#[test]
fn myers_diff_empty_sequences() {
    let ops = myers_diff(&[], &[]);
    assert!(ops.is_empty());
}

#[test]
fn myers_diff_insert_only() {
    let ops = myers_diff(&[], &["a", "b"]);
    assert_eq!(ops.len(), 2);
    assert!(ops.iter().all(|(op, _)| *op == DiffOp::Insert));
}

#[test]
fn myers_diff_delete_only() {
    let ops = myers_diff(&["a", "b"], &[]);
    assert_eq!(ops.len(), 2);
    assert!(ops.iter().all(|(op, _)| *op == DiffOp::Delete));
}

#[test]
fn myers_diff_equal() {
    let ops = myers_diff(&["a", "b", "c"], &["a", "b", "c"]);
    assert_eq!(ops.len(), 3);
    assert!(ops.iter().all(|(op, _)| *op == DiffOp::Equal));
}

#[test]
fn execution_policy_rejects_process_exec_when_read_only() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        capabilities: BTreeMap::from([("process".to_string(), vec!["exec".to_string()])]),
        ..Default::default()
    });
    let result = enforce_current_policy_for_builtin("exec", &[]);
    pop_execution_policy();
    assert!(result.is_err());
}

#[test]
fn execution_policy_rejects_unlisted_tool() {
    push_execution_policy(CapabilityPolicy {
        tools: vec!["read".to_string()],
        ..Default::default()
    });
    let result = enforce_current_policy_for_tool("edit");
    pop_execution_policy();
    assert!(result.is_err());
}

#[test]
fn normalize_run_record_preserves_trace_spans() {
    let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "_type": "run_record",
        "id": "run_trace",
        "workflow_id": "wf",
        "status": "completed",
        "started_at": "1",
        "trace_spans": [
            {
                "span_id": 1,
                "parent_id": null,
                "kind": "pipeline",
                "name": "workflow",
                "start_ms": 0,
                "duration_ms": 42,
                "metadata": {"model": "demo"}
            }
        ]
    }));

    let run = normalize_run_record(&value).unwrap();
    assert_eq!(run.trace_spans.len(), 1);
    assert_eq!(run.trace_spans[0].kind, "pipeline");
    assert_eq!(
        run.trace_spans[0].metadata["model"],
        serde_json::json!("demo")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn pre_tool_hook_deny_blocks_execution() {
    clear_tool_hooks();
    register_tool_hook(ToolHook {
        pattern: "dangerous_*".to_string(),
        pre: Some(Rc::new(|_name, _args| {
            PreToolAction::Deny("blocked by policy".to_string())
        })),
        post: None,
    });
    let result = run_pre_tool_hooks("dangerous_delete", &serde_json::json!({}))
        .await
        .expect("hook result");
    clear_tool_hooks();
    assert!(matches!(result, PreToolAction::Deny(_)));
}

#[tokio::test(flavor = "current_thread")]
async fn pre_tool_hook_allow_passes_through() {
    clear_tool_hooks();
    register_tool_hook(ToolHook {
        pattern: "safe_*".to_string(),
        pre: Some(Rc::new(|_name, _args| PreToolAction::Allow)),
        post: None,
    });
    let result = run_pre_tool_hooks("safe_read", &serde_json::json!({}))
        .await
        .expect("hook result");
    clear_tool_hooks();
    assert!(matches!(result, PreToolAction::Allow));
}

#[tokio::test(flavor = "current_thread")]
async fn pre_tool_hook_modify_rewrites_args() {
    clear_tool_hooks();
    register_tool_hook(ToolHook {
        pattern: "*".to_string(),
        pre: Some(Rc::new(|_name, _args| {
            PreToolAction::Modify(serde_json::json!({"path": "/sanitized"}))
        })),
        post: None,
    });
    let result = run_pre_tool_hooks("read_file", &serde_json::json!({"path": "/etc/passwd"}))
        .await
        .expect("hook result");
    clear_tool_hooks();
    match result {
        PreToolAction::Modify(args) => assert_eq!(args["path"], "/sanitized"),
        _ => panic!("expected Modify"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn post_tool_hook_modifies_result() {
    clear_tool_hooks();
    register_tool_hook(ToolHook {
        pattern: "exec".to_string(),
        pre: None,
        post: Some(Rc::new(|_name, result| {
            if result.contains("SECRET") {
                PostToolAction::Modify("[REDACTED]".to_string())
            } else {
                PostToolAction::Pass
            }
        })),
    });
    let result = run_post_tool_hooks("exec", &serde_json::json!({}), "output with SECRET data")
        .await
        .expect("hook result");
    let clean = run_post_tool_hooks("exec", &serde_json::json!({}), "clean output")
        .await
        .expect("hook result");
    clear_tool_hooks();
    assert_eq!(result, "[REDACTED]");
    assert_eq!(clean, "clean output");
}

#[tokio::test(flavor = "current_thread")]
async fn unmatched_hook_pattern_does_not_fire() {
    clear_tool_hooks();
    register_tool_hook(ToolHook {
        pattern: "exec".to_string(),
        pre: Some(Rc::new(|_name, _args| {
            PreToolAction::Deny("should not match".to_string())
        })),
        post: None,
    });
    let result = run_pre_tool_hooks("read_file", &serde_json::json!({}))
        .await
        .expect("hook result");
    clear_tool_hooks();
    assert!(matches!(result, PreToolAction::Allow));
}

#[test]
fn glob_match_patterns() {
    assert!(glob_match("*", "anything"));
    assert!(glob_match("exec*", "exec_at"));
    assert!(glob_match("*_file", "read_file"));
    assert!(!glob_match("exec*", "read_file"));
    assert!(glob_match("read_file", "read_file"));
    assert!(!glob_match("read_file", "write_file"));
}

#[test]
fn microcompact_snips_large_output() {
    let large = "x".repeat(50_000);
    let result = microcompact_tool_output(&large, 10_000);
    assert!(result.len() < 15_000);
    assert!(result.contains("snipped"));
}

#[test]
fn microcompact_preserves_small_output() {
    let small = "hello world";
    let result = microcompact_tool_output(small, 10_000);
    assert_eq!(result, small);
}

#[test]
fn microcompact_preserves_strong_keyword_lines_without_file_line() {
    // Strong keywords ("FAIL", "panic") must preserve the line on their own
    // even without a file:line anchor — they appear on narrative lines (Go
    // "--- FAIL: TestName", Rust "thread '...' panicked at ...",
    // pytest "FAILED tests/..."). Language-specific patterns stay out of the
    // VM; only the generic "strong keyword without file:line" rule lives here.
    let mut output = String::new();
    for i in 0..100 {
        output.push_str(&format!("verbose progress line {i}\n"));
    }
    output.push_str("--- FAIL: TestEmpty (0.00s)\n");
    output.push_str("thread 'tests::test_foo' panicked at src/lib.rs:42:5\n");
    output.push_str("FAILED tests/test_parser.py::test_empty\n");
    for i in 0..100 {
        output.push_str(&format!("more output after failures {i}\n"));
    }
    let result = microcompact_tool_output(&output, 2_000);
    assert!(
        result.contains("--- FAIL: TestEmpty"),
        "strong 'FAIL' keyword should preserve the line:\n{result}"
    );
    assert!(
        result.contains("panicked at"),
        "strong 'panic' keyword should preserve the line:\n{result}"
    );
    assert!(
        result.contains("FAILED tests/test_parser.py"),
        "strong 'FAIL' keyword should preserve pytest-style lines too:\n{result}"
    );
}

#[test]
fn auto_compact_messages_reduces_count() {
    let mut messages: Vec<serde_json::Value> = (0..20)
        .map(|i| serde_json::json!({"role": "user", "content": format!("message {i}")}))
        .collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let compacted = runtime.block_on(auto_compact_messages(
        &mut messages,
        &AutoCompactConfig {
            compact_strategy: CompactStrategy::Truncate,
            keep_last: 6,
            ..Default::default()
        },
        None,
    ));
    let summary = compacted.unwrap();
    assert!(summary.is_some());
    assert!(messages.len() <= 7);
    assert!(messages[0]["content"]
        .as_str()
        .unwrap()
        .contains("auto-compacted"));
}

#[test]
fn auto_compact_noop_when_under_threshold() {
    let mut messages: Vec<serde_json::Value> = (0..4)
        .map(|i| serde_json::json!({"role": "user", "content": format!("msg {i}")}))
        .collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let compacted = runtime.block_on(auto_compact_messages(
        &mut messages,
        &AutoCompactConfig {
            compact_strategy: CompactStrategy::Truncate,
            keep_last: 6,
            ..Default::default()
        },
        None,
    ));
    assert!(compacted.unwrap().is_none());
    assert_eq!(messages.len(), 4);
}

#[test]
fn observation_mask_preserves_errors_masks_verbose_output() {
    let verbose_lines: Vec<String> = (0..60)
        .map(|i| format!("// source line {} of the generated file", i))
        .collect();
    let verbose_content = format!(
        "File created: a.go\npackage main\n{}",
        verbose_lines.join("\n")
    );
    let mut messages = vec![
        serde_json::json!({"role": "assistant", "content": "I'll create the file now."}),
        serde_json::json!({"role": "user", "content": verbose_content}),
        serde_json::json!({"role": "assistant", "content": "Now let me run the tests."}),
        serde_json::json!({"role": "user", "content": "error: cannot find module\nexit code 1\nfailed to compile"}),
        serde_json::json!({"role": "assistant", "content": "I see the issue. Let me fix it."}),
        serde_json::json!({"role": "user", "content": "File patched successfully."}),
        serde_json::json!({"role": "assistant", "content": "Running tests again."}),
        serde_json::json!({"role": "user", "content": "All tests passed."}),
    ];
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let compacted = runtime.block_on(auto_compact_messages(
        &mut messages,
        &AutoCompactConfig {
            compact_strategy: CompactStrategy::ObservationMask,
            keep_last: 2,
            ..Default::default()
        },
        None,
    ));
    let summary = compacted.unwrap().unwrap();
    assert!(summary.contains("I'll create the file now."));
    assert!(summary.contains("Now let me run the tests."));
    assert!(summary.contains("I see the issue. Let me fix it."));
    assert!(summary.contains("error: cannot find module"));
    assert!(summary.contains("exit code 1"));
    assert!(summary.contains("masked]"));
    assert!(summary.contains("File created: a.go"));
    assert!(!summary.contains("File patched successfully."));
    assert!(!summary.contains("Running tests again."));
    assert!(!summary.contains("All tests passed."));
    assert_eq!(messages.len(), 4);
}

#[test]
fn observation_mask_keeps_short_tool_output() {
    let messages = vec![
        serde_json::json!({"role": "user", "content": "OK"}),
        serde_json::json!({"role": "user", "content": "Done."}),
    ];
    let summary = observation_mask_compaction(&messages, 2);
    assert!(summary.contains("[user] OK"));
    assert!(summary.contains("[user] Done."));
    assert!(!summary.contains("masked"));
}

#[test]
fn estimate_message_tokens_basic() {
    let messages = vec![
        serde_json::json!({"role": "user", "content": "a".repeat(400)}),
        serde_json::json!({"role": "assistant", "content": "b".repeat(400)}),
    ];
    let tokens = estimate_message_tokens(&messages);
    assert_eq!(tokens, 200);
}

#[test]
fn dedup_artifacts_removes_duplicates() {
    let mut artifacts = vec![
        ArtifactRecord {
            id: "a1".to_string(),
            kind: "test".to_string(),
            text: Some("duplicate content".to_string()),
            ..Default::default()
        },
        ArtifactRecord {
            id: "a2".to_string(),
            kind: "test".to_string(),
            text: Some("duplicate content".to_string()),
            ..Default::default()
        },
        ArtifactRecord {
            id: "a3".to_string(),
            kind: "test".to_string(),
            text: Some("unique content".to_string()),
            ..Default::default()
        },
    ];
    dedup_artifacts(&mut artifacts);
    assert_eq!(artifacts.len(), 2);
}

#[test]
fn microcompact_artifact_snips_oversized() {
    let mut artifact = ArtifactRecord {
        id: "a1".to_string(),
        kind: "test".to_string(),
        text: Some("x".repeat(10_000)),
        estimated_tokens: Some(2_500),
        ..Default::default()
    };
    microcompact_artifact(&mut artifact, 500);
    assert!(artifact.text.as_ref().unwrap().len() < 5_000);
    assert_eq!(artifact.estimated_tokens, Some(500));
}

#[test]
fn select_artifacts_adaptive_drops_stale_evidence_after_fresh_write() {
    let selected = select_artifacts_adaptive(
        vec![
            ArtifactRecord {
                id: "research-index".to_string(),
                kind: "summary".to_string(),
                text: Some("index.ts currently exports only authGuard".to_string()),
                freshness: Some("normal".to_string()),
                metadata: BTreeMap::from([(
                    "evidence_paths".to_string(),
                    serde_json::json!(["packages/server/src/middleware/index.ts"]),
                )]),
                ..Default::default()
            },
            ArtifactRecord {
                id: "research-api".to_string(),
                kind: "summary".to_string(),
                text: Some("api.ts currently uses withMiddleware".to_string()),
                freshness: Some("normal".to_string()),
                metadata: BTreeMap::from([(
                    "evidence_paths".to_string(),
                    serde_json::json!(["packages/server/src/routes/api.ts"]),
                )]),
                ..Default::default()
            },
            ArtifactRecord {
                id: "batch-2".to_string(),
                kind: "summary".to_string(),
                text: Some("Updated middleware/index.ts to export rateLimit".to_string()),
                freshness: Some("fresh".to_string()),
                metadata: BTreeMap::from([(
                    "changed_paths".to_string(),
                    serde_json::json!(["packages/server/src/middleware/index.ts"]),
                )]),
                ..Default::default()
            },
        ],
        &ContextPolicy::default(),
    );
    let ids: Vec<_> = selected
        .iter()
        .map(|artifact| artifact.id.as_str())
        .collect();
    assert!(!ids.contains(&"research-index"), "ids={ids:?}");
    assert!(ids.contains(&"research-api"), "ids={ids:?}");
    assert!(ids.contains(&"batch-2"), "ids={ids:?}");
}

#[test]
fn arg_constraint_allows_matching_pattern() {
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "exec".to_string(),
            arg_patterns: vec!["cargo *".to_string()],
            arg_key: Some("command".to_string()),
        }],
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "exec",
        &serde_json::json!({"command": "cargo test"}),
    );
    assert!(result.is_ok());
}

#[test]
fn arg_constraint_rejects_non_matching_pattern() {
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "exec".to_string(),
            arg_patterns: vec!["cargo *".to_string()],
            arg_key: Some("command".to_string()),
        }],
        ..Default::default()
    };
    let result =
        enforce_tool_arg_constraints(&policy, "exec", &serde_json::json!({"command": "rm -rf /"}));
    assert!(result.is_err());
}

#[test]
fn arg_constraint_ignores_unmatched_tool() {
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "exec".to_string(),
            arg_patterns: vec!["cargo *".to_string()],
            arg_key: Some("command".to_string()),
        }],
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "read_file",
        &serde_json::json!({"path": "/etc/passwd"}),
    );
    assert!(result.is_ok());
}

#[test]
fn arg_constraint_prefers_declared_path_param_annotations() {
    let mut tool_annotations = std::collections::BTreeMap::new();
    tool_annotations.insert(
        "edit".to_string(),
        crate::tool_annotations::ToolAnnotations {
            kind: crate::tool_annotations::ToolKind::Edit,
            arg_schema: crate::tool_annotations::ToolArgSchema {
                path_params: vec!["path".to_string()],
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "edit".to_string(),
            arg_patterns: vec!["tests/*".to_string()],
            arg_key: None,
        }],
        tool_annotations,
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "edit",
        &serde_json::json!({
            "action": "replace_range",
            "path": "tests/unit/test_experiment_service.py",
            "content": "..."
        }),
    );
    assert!(result.is_ok());
}

#[test]
fn arg_constraint_without_arg_key_or_metadata_skips_with_warning() {
    // Regression: a heuristic fallback used to pick the first string arg
    // (often `action`) and blame it for mismatches. Policy authors now must
    // declare `arg_key` or `path_params`; otherwise the constraint is
    // SKIPPED with a structured `log_warn`.
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "edit".to_string(),
            arg_patterns: vec!["tests/unit/test_experiment_service.py".to_string()],
            arg_key: None,
        }],
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "edit",
        &serde_json::json!({
            "action": "exact_patch",
            "path": "tests/unit/test_experiment_service.py",
            "old_string": "assert len(items) == 1",
            "new_string": "assert len(items) == 2",
        }),
    );
    assert!(
        result.is_ok(),
        "unresolved constraint must skip (not reject) so a misconfigured policy doesn't silently block work; got: {result:?}"
    );
}

#[test]
fn arg_constraint_with_explicit_arg_key_allows_matching_path() {
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "edit".to_string(),
            arg_patterns: vec!["tests/unit/*".to_string()],
            arg_key: Some("path".to_string()),
        }],
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "edit",
        &serde_json::json!({
            "action": "exact_patch",
            "path": "tests/unit/test_experiment_service.py",
        }),
    );
    assert!(
        result.is_ok(),
        "expected allow (path matches), got: {result:?}"
    );
}

#[test]
fn arg_constraint_error_names_the_path_key_not_the_action_value() {
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "edit".to_string(),
            arg_patterns: vec!["src/allowed/*".to_string()],
            arg_key: Some("path".to_string()),
        }],
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "edit",
        &serde_json::json!({
            "action": "replace_range",
            "path": "src/forbidden/foo.rs",
            "content": "..."
        }),
    );
    let Err(err) = result else {
        panic!("expected rejection, got Ok");
    };
    let msg = format!("{err:?}");
    assert!(
        msg.contains("path 'src/forbidden/foo.rs'"),
        "error should name the `path` argument, got: {msg}"
    );
    assert!(
        !msg.contains("argument 'replace_range'"),
        "error must not blame the `action` value, got: {msg}"
    );
}

#[test]
fn arg_constraint_skips_when_no_path_key_present_in_call() {
    // Absence of the declared arg_key is outside the allow-list's scope —
    // skip rather than rejecting an empty string against the patterns.
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "edit".to_string(),
            arg_patterns: vec!["tests/*".to_string()],
            arg_key: Some("path".to_string()),
        }],
        ..Default::default()
    };
    let result = enforce_tool_arg_constraints(
        &policy,
        "edit",
        &serde_json::json!({
            "action": "noop",
            "content": "...",
        }),
    );
    assert!(
        result.is_ok(),
        "no path arg → constraint should skip, got: {result:?}"
    );
}

#[test]
fn microcompact_handles_multibyte_utf8() {
    // Slicing at arbitrary byte offsets would panic; these three scripts cover
    // 4/2/3-byte sequences respectively.
    let emoji_output = "🔥".repeat(500);
    let result = microcompact_tool_output(&emoji_output, 400);
    assert!(result.contains("snipped"));

    let mixed = format!("{}{}{}", "a".repeat(300), "é".repeat(500), "b".repeat(300));
    let result2 = microcompact_tool_output(&mixed, 400);
    assert!(result2.contains("snipped"));

    let cjk = "中文".repeat(500);
    let result3 = microcompact_tool_output(&cjk, 400);
    assert!(result3.contains("snipped"));
}

#[test]
fn workflow_node_defaults_exit_when_verified_to_false() {
    let node = WorkflowNode::default();
    assert!(!node.exit_when_verified);
}

#[test]
fn workflow_node_exit_when_verified_round_trips_through_serde() {
    let node = WorkflowNode {
        id: Some("execute".to_string()),
        kind: "stage".to_string(),
        exit_when_verified: true,
        ..Default::default()
    };
    let encoded = serde_json::to_value(&node).expect("serialize");
    assert_eq!(
        encoded.get("exit_when_verified"),
        Some(&serde_json::json!(true))
    );
    let decoded: WorkflowNode = serde_json::from_value(encoded).expect("deserialize");
    assert!(decoded.exit_when_verified);
}

#[test]
fn workflow_node_exit_when_verified_accepts_missing_field_for_backcompat() {
    let encoded = serde_json::json!({
        "id": "legacy_stage",
        "kind": "stage",
    });
    let decoded: WorkflowNode = serde_json::from_value(encoded).expect("deserialize");
    assert!(
        !decoded.exit_when_verified,
        "nodes serialized before this field was added must deserialize with the default"
    );
}
