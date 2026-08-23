//! Persisting and reloading a run record.
//!
//! Saving materializes HITL questions from the active event log, redacts secrets
//! before anything reaches disk, turns artifacts into typed handoffs, and links
//! the run receipt onto the handoff. Reloading has to reproduce the observability
//! summary and the daemon events kept in the sidecar, trace spans survive
//! normalization, and a run diff reports the stage that changed.

use crate::event_log::EventLog;
use crate::orchestration::*;
use std::collections::BTreeMap;
#[test]
fn save_run_record_materializes_hitl_questions_from_active_event_log() {
    let temp_dir = tempfile::tempdir().unwrap();
    crate::event_log::reset_active_event_log();
    let log = crate::event_log::install_memory_for_current_thread(8);
    let topic = crate::event_log::Topic::new(crate::HITL_QUESTIONS_TOPIC).unwrap();
    futures::executor::block_on(
        log.append(
            &topic,
            crate::event_log::LogEvent::new(
                "hitl.question_asked",
                serde_json::json!({
                    "request_id": "hitl_question_1",
                    "kind": "question",
                    "agent": "planner",
                    "trace_id": "trace_1",
                    "run_id": "run_hitl",
                    "requested_at": "2026-04-23T12:00:00Z",
                    "payload": {
                        "prompt": "Which environment should I deploy to?"
                    }
                }),
            )
            .with_headers(std::collections::BTreeMap::from([
                ("request_id".to_string(), "hitl_question_1".to_string()),
                ("trace_id".to_string(), "trace_1".to_string()),
                ("run_id".to_string(), "run_hitl".to_string()),
            ])),
        ),
    )
    .unwrap();

    let path = temp_dir.path().join("run.json");
    let run = RunRecord {
        id: "run_hitl".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        ..Default::default()
    };

    save_run_record(&run, Some(path.to_str().unwrap())).unwrap();
    let loaded = load_run_record(&path).unwrap();

    assert_eq!(loaded.hitl_questions.len(), 1);
    assert_eq!(
        loaded.hitl_questions[0].prompt,
        "Which environment should I deploy to?"
    );

    crate::event_log::reset_active_event_log();
}

#[test]
fn save_run_record_redacts_secrets_before_persisting() {
    crate::reset_thread_local_state();
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("run.json");
    let run = RunRecord {
        id: "run_redaction".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        stages: vec![RunStageRecord {
            id: "stage_secret".to_string(),
            node_id: "stage".to_string(),
            kind: "tool".to_string(),
            status: "completed".to_string(),
            outcome: "ok".to_string(),
            visible_text: Some(
                "https://user:password@example.com/cb?access_token=raw-stage-token".to_string(),
            ),
            metadata: BTreeMap::from([(
                "api_key".to_string(),
                serde_json::json!("raw-stage-api-key"),
            )]),
            ..Default::default()
        }],
        metadata: BTreeMap::from([
            ("api_key".to_string(), serde_json::json!("raw-run-api-key")),
            (
                "callback_url".to_string(),
                serde_json::json!(
                    "https://user:password@example.com/items?client_secret=raw-client-secret&ok=1"
                ),
            ),
        ]),
        ..Default::default()
    };

    save_run_record(&run, Some(path.to_str().unwrap())).unwrap();

    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("[redacted]") || raw.contains("%5Bredacted%5D"));
    for secret in [
        "raw-run-api-key",
        "raw-stage-api-key",
        "user:password",
        "raw-stage-token",
        "raw-client-secret",
    ] {
        assert!(
            !raw.contains(secret),
            "run record persisted secret {secret}: {raw}"
        );
    }

    let loaded = load_run_record(&path).unwrap();
    assert_eq!(
        loaded.metadata.get("api_key"),
        Some(&serde_json::json!("[redacted]"))
    );
}

#[test]
fn normalize_run_record_materializes_typed_handoffs_from_artifacts() {
    let value = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "_type": "run_record",
        "id": "run_handoff",
        "workflow_id": "wf",
        "status": "completed",
        "artifacts": [{
            "_type": "artifact",
            "id": "artifact_handoff",
            "kind": "handoff",
            "data": {
                "_type": "handoff_artifact",
                "id": "handoff_1",
                "source_persona": "merge_captain",
                "target_persona_or_human": {
                    "kind": "persona",
                    "label": "review_captain"
                },
                "task": "Review PR #461",
                "reason": "Explicit review is required before merge",
                "requested_capabilities": ["review"],
                "allowed_side_effects": ["comment_on_pr"]
            }
        }]
    }));

    let run = normalize_run_record(&value).expect("normalize run");

    assert_eq!(run.handoffs.len(), 1);
    assert_eq!(run.handoffs[0].source_persona, "merge_captain");
    assert_eq!(
        run.handoffs[0].target_persona_or_human.display_name(),
        "review_captain"
    );
    assert_eq!(
        run.artifacts[0]
            .metadata
            .get("handoff_id")
            .and_then(|value| value.as_str()),
        Some("handoff_1")
    );
}

#[test]
fn save_run_record_adds_run_receipt_link_to_handoff() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("run.json");
    let run = RunRecord {
        id: "run_handoff_receipt".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        handoffs: vec![HandoffArtifact {
            id: "handoff_receipt".to_string(),
            source_persona: "merge_captain".to_string(),
            target_persona_or_human: HandoffTargetRecord {
                kind: "human".to_string(),
                id: None,
                label: Some("maintainer".to_string()),
                uri: None,
            },
            task: "Approve the rollout".to_string(),
            reason: "Human sign-off gates production changes".to_string(),
            requested_capabilities: vec!["release_signoff".to_string()],
            allowed_side_effects: vec!["publish_release_notes".to_string()],
            ..Default::default()
        }],
        ..Default::default()
    };

    save_run_record(&run, Some(path.to_str().expect("path"))).expect("save run");
    let loaded = load_run_record(&path).expect("load run");

    assert_eq!(loaded.handoffs.len(), 1);
    assert!(loaded.handoffs[0].receipt_links.iter().any(|link| {
        link.kind == "run_receipt"
            && link.run_id.as_deref() == Some("run_handoff_receipt")
            && link.path.as_deref() == Some(path.to_str().expect("path"))
    }));
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
    std::fs::write(sidecar_dir.join("llm_transcript.jsonl"), "{}\n").unwrap();

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
fn save_run_record_normalizes_external_transcript_into_movable_bundle() {
    let temp = tempfile::tempdir().unwrap();
    let source_dir = tempfile::tempdir().unwrap();
    let original_bundle = temp.path().join("original");
    std::fs::create_dir_all(&original_bundle).unwrap();
    let run_path = original_bundle.join("run.json");
    let source_path = source_dir.path().join("llm_transcript.jsonl");
    let transcript = concat!(
        "{\"type\":\"system_prompt\",\"session_id\":\"session-1\"}\n",
        "{\"type\":\"agent_session_finalized\",\"terminal\":true,\"final_status\":\"done\"}\n",
    );
    std::fs::write(&source_path, transcript).unwrap();
    let run = RunRecord {
        id: "run_external_transcript".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        ..Default::default()
    };

    save_run_record_with_transcript(&run, Some(run_path.to_str().unwrap()), Some(&source_path))
        .unwrap();

    let canonical = original_bundle.join("run-llm/llm_transcript.jsonl");
    assert_eq!(std::fs::read_to_string(&canonical).unwrap(), transcript);
    let persisted = load_run_record(&run_path).unwrap();
    let pointer_path = verified_llm_transcript_pointer_path(&persisted, &run_path).unwrap();
    assert_eq!(pointer_path, canonical);

    let moved_bundle = temp.path().join("moved");
    std::fs::rename(&original_bundle, &moved_bundle).unwrap();
    let moved_run_path = moved_bundle.join("run.json");
    let moved = load_run_record(&moved_run_path).unwrap();
    let moved_pointer = verified_llm_transcript_pointer_path(&moved, &moved_run_path).unwrap();
    assert_eq!(
        moved_pointer,
        moved_bundle.join("run-llm/llm_transcript.jsonl")
    );
}

#[test]
fn save_run_record_omits_missing_model_transcript_pointer() {
    let temp = tempfile::tempdir().unwrap();
    let run_path = temp.path().join("run.json");
    let missing = temp.path().join("elsewhere/llm_transcript.jsonl");
    let run = RunRecord {
        id: "run_missing_transcript".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        ..Default::default()
    };

    save_run_record_with_transcript(&run, Some(run_path.to_str().unwrap()), Some(&missing))
        .unwrap();
    let loaded = load_run_record(&run_path).unwrap();
    assert!(
        loaded
            .observability
            .unwrap()
            .transcript_pointers
            .iter()
            .all(|pointer| pointer.kind != "llm_jsonl"),
        "a missing transcript must be represented by the absence of a pointer"
    );
}

#[test]
fn save_run_record_survives_malformed_model_transcript_without_a_pointer() {
    let temp = tempfile::tempdir().unwrap();
    let run_path = temp.path().join("run.json");
    let malformed = temp.path().join("llm_transcript.jsonl");
    std::fs::write(&malformed, "not-json\n").unwrap();
    let run = RunRecord {
        id: "run_malformed_transcript".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        ..Default::default()
    };

    save_run_record_with_transcript(&run, Some(run_path.to_str().unwrap()), Some(&malformed))
        .expect("transcript observability must not block the owning run record");
    let loaded = load_run_record(&run_path).unwrap();
    assert!(loaded
        .observability
        .unwrap()
        .transcript_pointers
        .iter()
        .all(|pointer| pointer.kind != "llm_jsonl"));
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
        crate::orchestration::DaemonEventKindRecord::Spawned
    );
    assert_eq!(
        observability.daemon_events[1].payload_summary.as_deref(),
        Some("new review requested")
    );
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
