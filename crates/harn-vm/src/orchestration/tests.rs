//! Orchestration integration tests for policy/workflow/mutation-session.

use super::records::{myers_diff, DiffOp};
use super::*;
use futures::StreamExt;
use std::collections::BTreeMap;
use std::sync::Arc;

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
fn capability_intersection_narrows_read_only_roots_to_common_set() {
    let ceiling = CapabilityPolicy {
        workspace_roots: vec!["/work".to_string()],
        read_only_roots: vec!["/mnt/memory".to_string(), "/mnt/bundle".to_string()],
        ..Default::default()
    };
    let requested = CapabilityPolicy {
        workspace_roots: vec!["/work".to_string()],
        read_only_roots: vec!["/mnt/memory".to_string(), "/mnt/other".to_string()],
        ..Default::default()
    };
    let merged = ceiling.intersect(&requested).unwrap();
    assert_eq!(merged.read_only_roots, vec!["/mnt/memory".to_string()]);

    // An empty side defers to the other, mirroring workspace_roots.
    let unbounded = CapabilityPolicy::default();
    let merged = unbounded.intersect(&requested).unwrap();
    assert_eq!(
        merged.read_only_roots,
        vec!["/mnt/memory".to_string(), "/mnt/other".to_string()]
    );
}

#[test]
fn capability_intersection_narrows_process_sandbox_policy() {
    let ceiling = CapabilityPolicy {
        process_sandbox: ProcessSandboxPolicy {
            presets: Some(vec![
                ProcessSandboxPreset::SystemRuntime,
                ProcessSandboxPreset::DeveloperToolchains,
            ]),
            read_roots: vec!["/opt/sdk".to_string()],
            write_roots: vec!["/opt/cache".to_string()],
        },
        ..Default::default()
    };
    let requested = CapabilityPolicy {
        process_sandbox: ProcessSandboxPolicy {
            presets: Some(vec![
                ProcessSandboxPreset::DeveloperToolchains,
                ProcessSandboxPreset::UserTemp,
            ]),
            read_roots: vec!["/opt/sdk".to_string(), "/opt/other".to_string()],
            write_roots: vec!["/opt/cache".to_string(), "/opt/other-cache".to_string()],
        },
        ..Default::default()
    };

    let merged = ceiling.intersect(&requested).unwrap();
    assert_eq!(
        merged.process_sandbox.presets,
        Some(vec![ProcessSandboxPreset::DeveloperToolchains])
    );
    assert_eq!(
        merged.process_sandbox.read_roots,
        vec!["/opt/sdk".to_string()]
    );
    assert_eq!(
        merged.process_sandbox.write_roots,
        vec!["/opt/cache".to_string()]
    );
}

#[test]
fn process_sandbox_defaults_include_package_manager_config() {
    assert!(
        ProcessSandboxPolicy::default()
            .effective_presets()
            .contains(&ProcessSandboxPreset::PackageManagerConfig),
        "package-manager config roots should be part of the default process sandbox presets"
    );
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
    assert_eq!(
        crate::tool_surface::tool_names_from_spec(&node.tools),
        vec!["read", "run"]
    );
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
        handoffs: vec![],
        policy: CapabilityPolicy::default(),
        execution: None,
        transcript: None,
        usage: None,
        replay_fixture: None,
        observability: None,
        trace_spans: vec![],
        tool_recordings: vec![],
        hitl_questions: vec![],
        persona_runtime: vec![],
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
            bad,
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
fn clarifying_question_eval_requires_matching_hitl_prompt() {
    let run = RunRecord {
        id: "run_clarify".to_string(),
        workflow_id: "wf".to_string(),
        status: "completed".to_string(),
        hitl_questions: vec![RunHitlQuestionRecord {
            request_id: "hitl_question_1".to_string(),
            prompt: "Which repository should I patch?".to_string(),
            agent: "planner".to_string(),
            trace_id: Some("trace-1".to_string()),
            asked_at: "2026-04-23T12:00:00Z".to_string(),
        }],
        ..Default::default()
    };
    let fixture = ReplayFixture {
        type_name: "replay_fixture".to_string(),
        id: "fixture_clarify".to_string(),
        source_run_id: run.id.clone(),
        workflow_id: run.workflow_id.clone(),
        created_at: "2026-04-23T12:00:01Z".to_string(),
        eval_kind: Some("clarifying_question".to_string()),
        clarifying_question: Some(ClarifyingQuestionEvalSpec {
            required_terms: vec!["repository".to_string()],
            forbidden_terms: vec!["branch".to_string()],
            ..Default::default()
        }),
        expected_status: "completed".to_string(),
        stage_assertions: vec![],
        ..Default::default()
    };

    let report = evaluate_run_against_fixture(&run, &fixture);

    assert!(report.pass, "failures: {:?}", report.failures);
}

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
fn execution_policy_allows_llm_call_under_read_only_side_effect_ceiling() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        capabilities: BTreeMap::from([("llm".to_string(), vec!["call".to_string()])]),
        ..Default::default()
    });
    let result = enforce_current_policy_for_builtin("llm_call", &[]);
    pop_execution_policy();
    assert!(result.is_ok());
}

#[test]
fn execution_policy_rejects_llm_call_without_llm_capability() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("network".to_string()),
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        ..Default::default()
    });
    let result = enforce_current_policy_for_builtin("llm_call", &[]);
    pop_execution_policy();
    assert!(result.is_err());
}

#[test]
fn reset_thread_local_state_clears_execution_policy_stack() {
    push_execution_policy(CapabilityPolicy {
        side_effect_level: Some("read_only".to_string()),
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        ..Default::default()
    });
    assert!(current_execution_policy().is_some());
    crate::reset_thread_local_state();
    assert!(current_execution_policy().is_none());
}

#[test]
fn execution_policy_rejects_unlisted_tool() {
    use crate::agent_events::DenialGate;
    push_execution_policy(CapabilityPolicy {
        tools: vec!["read".to_string()],
        ..Default::default()
    });
    let denial = enforce_current_policy_for_tool("edit").unwrap_err();
    pop_execution_policy();
    // The refusal is structured: gate identifies the tool ceiling and the
    // reason carries the same text the model sees (#2780).
    assert_eq!(denial.gate, DenialGate::ToolCeiling);
    assert!(denial.capability.is_none());
    assert!(denial.reason.contains("tool ceiling"));
}

#[test]
fn execution_policy_capability_ceiling_records_gate_and_capability() {
    use crate::agent_events::DenialGate;
    let mut tool_annotations = BTreeMap::new();
    tool_annotations.insert(
        "edit".to_string(),
        crate::tool_annotations::ToolAnnotations {
            kind: crate::tool_annotations::ToolKind::Edit,
            capabilities: BTreeMap::from([(
                "workspace".to_string(),
                vec!["write_text".to_string()],
            )]),
            ..Default::default()
        },
    );
    push_execution_policy(CapabilityPolicy {
        tools: vec!["edit".to_string()],
        capabilities: BTreeMap::from([("workspace".to_string(), vec!["read_text".to_string()])]),
        tool_annotations,
        ..Default::default()
    });
    let denial = enforce_current_policy_for_tool("edit").unwrap_err();
    pop_execution_policy();
    assert_eq!(denial.gate, DenialGate::CapabilityCeiling);
    assert_eq!(denial.capability.as_deref(), Some("workspace.write_text"));
}

#[test]
fn arg_constraint_denial_records_arg_constraint_gate() {
    use crate::agent_events::DenialGate;
    let policy = CapabilityPolicy {
        tool_arg_constraints: vec![ToolArgConstraint {
            tool: "exec".to_string(),
            arg_patterns: vec!["cargo *".to_string()],
            arg_key: Some("command".to_string()),
        }],
        ..Default::default()
    };
    let denial =
        enforce_tool_arg_constraints(&policy, "exec", &serde_json::json!({"command": "rm -rf /"}))
            .unwrap_err();
    assert_eq!(denial.gate, DenialGate::ArgConstraint);
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
        pre: Some(Arc::new(|_name, _args| {
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
        pre: Some(Arc::new(|_name, _args| PreToolAction::Allow)),
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
        pre: Some(Arc::new(|_name, _args| {
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
        post: Some(Arc::new(|_name, result: &str| {
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
        pre: Some(Arc::new(|_name, _args| {
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

#[tokio::test(flavor = "current_thread")]
async fn lifecycle_hook_patterns_match_payload_shapes() {
    clear_runtime_hooks();

    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let exports = vm
        .load_module_exports_from_source(
            "orchestration/tests/noop_hook.harn",
            "pub fn noop(event) { return nil }\n",
        )
        .await
        .expect("compile noop hook");
    let closure = exports.get("noop").expect("noop export").clone();

    for pattern in [
        "trigger.script_*",
        "trigger.provider == 'cron'",
        "trigger.kind =~ '^schedule'",
        "trigger.provider != 'webhook'",
        "script.path",
        "trigger.kind =~ '['",
    ] {
        register_vm_hook(
            HookEvent::PreAgentTurn,
            pattern,
            format!("test::{pattern}"),
            std::sync::Arc::clone(&closure),
        );
    }

    let payload = serde_json::json!({
        "target": "trigger.script_001",
        "trigger": {
            "provider": "cron",
            "kind": "schedule.tick",
        },
        "script": {
            "path": "scripts/bench_001.harn",
        },
    });

    assert_eq!(
        matching_vm_lifecycle_hooks(HookEvent::PreAgentTurn, &payload).len(),
        5,
        "valid glob, equality, regex, inequality, and truthy-path patterns should match"
    );
    clear_runtime_hooks();
}

#[tokio::test(flavor = "current_thread")]
async fn pipeline_on_finish_callback_replaces_return_value() {
    crate::reset_thread_local_state();

    let local = tokio::task::LocalSet::new();
    let value = local
        .run_until(async {
            let mut vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut vm);
            let chunk = crate::compile_source(
                "pipeline default() { pipeline_on_finish({ _h, v -> v + 100 }); return 7 }",
            )
            .expect("compile");
            vm.execute(&chunk).await.expect("execute")
        })
        .await;

    match value {
        VmValue::Int(n) => assert_eq!(n, 107, "on_finish callback should add 100 to 7"),
        other => panic!("expected Int, got {}", other.type_name()),
    }

    // One-shot consumption: a second execute without re-registration must
    // not re-invoke the previous callback.
    let next = local
        .run_until(async {
            let mut vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut vm);
            let chunk = crate::compile_source("pipeline default() { return 7 }").expect("compile");
            vm.execute(&chunk).await.expect("execute")
        })
        .await;
    match next {
        VmValue::Int(n) => assert_eq!(n, 7, "no callback registered → value passes through"),
        other => panic!("expected Int, got {}", other.type_name()),
    }
}

#[test]
fn pipeline_finish_events_round_trip_through_session_hook_parser() {
    for (input, expected) in [
        ("pre_finish", HookEvent::PreFinish),
        ("PreFinish", HookEvent::PreFinish),
        ("post_finish", HookEvent::PostFinish),
        ("PostFinish", HookEvent::PostFinish),
        ("on_unsettled_detected", HookEvent::OnUnsettledDetected),
        ("OnUnsettledDetected", HookEvent::OnUnsettledDetected),
    ] {
        let parsed = HookEvent::parse_session_event(input)
            .unwrap_or_else(|err| panic!("{input} should parse as a session event: {err}"));
        assert_eq!(parsed, expected, "{input} should map to {expected:?}");
    }
}

#[test]
fn lifecycle_hook_events_round_trip_through_session_hook_parser() {
    // harn#1859: the 7 new lifecycle events (suspend, resume, drain
    // phases) round-trip through the session-hook parser using both
    // their canonical PascalCase and Harn-facing snake_case spellings.
    for (input, expected) in [
        ("pre_suspend", HookEvent::PreSuspend),
        ("PreSuspend", HookEvent::PreSuspend),
        ("post_suspend", HookEvent::PostSuspend),
        ("PostSuspend", HookEvent::PostSuspend),
        ("pre_resume", HookEvent::PreResume),
        ("PreResume", HookEvent::PreResume),
        ("post_resume", HookEvent::PostResume),
        ("PostResume", HookEvent::PostResume),
        ("pre_drain", HookEvent::PreDrain),
        ("PreDrain", HookEvent::PreDrain),
        ("post_drain", HookEvent::PostDrain),
        ("PostDrain", HookEvent::PostDrain),
        ("on_drain_decision", HookEvent::OnDrainDecision),
        ("OnDrainDecision", HookEvent::OnDrainDecision),
    ] {
        let parsed = HookEvent::parse_session_event(input)
            .unwrap_or_else(|err| panic!("{input} should parse as a session event: {err}"));
        assert_eq!(parsed, expected, "{input} should map to {expected:?}");
    }
}

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
            token_threshold: 1,
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
fn auto_compact_noop_when_message_tokens_under_threshold() {
    let mut messages: Vec<serde_json::Value> = (0..20)
        .map(|i| serde_json::json!({"role": "user", "content": format!("short message {i}")}))
        .collect();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let compacted = runtime.block_on(auto_compact_messages(
        &mut messages,
        &AutoCompactConfig {
            compact_strategy: CompactStrategy::Truncate,
            token_threshold: 48_000,
            keep_last: 6,
            ..Default::default()
        },
        None,
    ));
    assert!(compacted.unwrap().is_none());
    assert_eq!(messages.len(), 20);
}

#[test]
fn observation_mask_preserves_errors_masks_verbose_output() {
    let verbose_lines: Vec<String> = (0..60)
        .map(|i| format!("// source line {i} of the generated file"))
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
            token_threshold: 1,
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

// --- Trusted-bridge scope for the runtime's own registered closures ---------
//
// The agent loop runs under an active execution policy. When the runtime
// invokes a closure IT registered (a reminder provider or a session hook) and
// that closure's body calls an app-provided bridged builtin, the call must be
// treated as a trusted bridge call rather than a model-issued tool call.
// Otherwise `enforce_current_policy_for_bridge_builtin` rejects it with
// "exceeds execution policy" and kills the turn — the regression these tests
// lock. Trust is narrowed to the runtime's own registered-closure seams, not
// the whole policy (see the negative control below).

/// Run `source` through a real VM under an active execution policy. The VM
/// exposes:
///   * `__probe_bridge_gate()` — a stand-in for an app-provided bridged
///     builtin. It runs the exact gate the model's bridged-builtin calls hit
///     at `dispatch.rs` (`enforce_current_policy_for_bridge_builtin`) using the
///     real Burin reminder builtin name from the bug, and counts each call.
///   * `__test_fire_reminders(session_id)` — drives the reminder-provider
///     evaluation seam (`evaluate_and_inject` -> `evaluate_vm_provider`).
///
/// Returns the script result (Ok output / Err message) and the probe count.
fn run_registered_closure_probe(source: &str) -> (Result<String, String>, usize) {
    use std::sync::atomic::{AtomicUsize, Ordering};

    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();

    let probe_calls = Arc::new(AtomicUsize::new(0));
    let probe_for_rt = Arc::clone(&probe_calls);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async move {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let chunk = crate::compile_source(source)?;
                let mut vm = crate::Vm::new();
                crate::stdlib::register_vm_stdlib(&mut vm);

                let probe = Arc::clone(&probe_for_rt);
                vm.register_builtin("__probe_bridge_gate", move |_args, _out| {
                    probe.fetch_add(1, Ordering::SeqCst);
                    crate::orchestration::enforce_current_policy_for_bridge_builtin(
                        "evaluate_burin_user_reminder_rules",
                    )?;
                    Ok(crate::value::VmValue::String(arcstr::ArcStr::from("ok")))
                });

                vm.register_async_builtin("__test_fire_reminders", |ctx, args| async move {
                    let session_id = args
                        .first()
                        .map(crate::value::VmValue::display)
                        .unwrap_or_default();
                    let report = crate::llm::reminder_providers::evaluate_and_inject(
                        Some(&ctx),
                        crate::orchestration::HookEvent::SessionStart,
                        &session_id,
                        serde_json::json!({}),
                        serde_json::json!({}),
                    )
                    .await?;
                    Ok(crate::json_to_vm_value(&report))
                });

                push_execution_policy(CapabilityPolicy {
                    tools: vec!["read".to_string()],
                    ..Default::default()
                });
                let outcome = vm.execute(&chunk).await;
                pop_execution_policy();
                outcome
                    .map(|_| vm.output().to_string())
                    .map_err(|error| error.to_string())
            })
            .await
    });

    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();
    (result, probe_calls.load(Ordering::SeqCst))
}

// A registered reminder provider AND a registered session hook, both whose
// bodies call a bridged builtin, must evaluate cleanly while the agent loop's
// execution policy is active. Before the trusted-bridge guard was added at the
// provider/hook invocation seams, the first such call died with
// `tool_rejected: ... exceeds execution policy`, taking down every turn.
#[test]
fn registered_provider_and_session_hook_evaluate_under_execution_policy() {
    let script = r#"pipeline main() {
  const session = agent_session_open("trusted-bridge-probe")
  agent_session_reset(session)
  register_reminder_provider({
    id: "probe-provider",
    subscribes_to: ["session_start"],
    evaluate: { ctx ->
      __probe_bridge_gate()
      return []
    },
  })
  register_session_hook("session_start", { _payload ->
    __probe_bridge_gate()
    return {control: "allow"}
  })
  __test_fire_reminders(session)
  __host_fire_session_hook("session_start", {session: {id: session}, event: "session_start"})
}"#;

    let (result, probe_calls) = run_registered_closure_probe(script);
    result.expect(
        "a registered reminder provider and session hook must evaluate under an active \
         execution policy without tripping the bridged-builtin gate",
    );
    assert_eq!(
        probe_calls, 2,
        "both the reminder-provider closure and the session-hook closure must have executed \
         the bridged-builtin probe exactly once each"
    );
}

// Negative control: a bridged builtin invoked OUTSIDE any registered
// provider/hook closure (i.e. at the top of the pipeline, the way a
// model-issued tool/builtin call reaches `dispatch.rs`) must STILL be rejected
// under the same policy. This proves the guard narrows trust to the runtime's
// own registered-closure seams rather than weakening the policy globally.
#[test]
fn bridged_builtin_outside_registered_closure_is_still_rejected_under_policy() {
    let script = r"pipeline main() {
  __probe_bridge_gate()
}";

    let (result, probe_calls) = run_registered_closure_probe(script);
    let error = result.expect_err(
        "a bridged builtin invoked outside a registered provider/hook closure must remain \
         rejected while an execution policy is active",
    );
    assert!(
        error.contains("exceeds execution policy"),
        "rejection must come from the execution-policy gate, got: {error}"
    );
    assert_eq!(
        probe_calls, 1,
        "the probe should have run once and been rejected by the gate"
    );
}

// --- Sibling-fn resolution survives the registering VM's teardown -----------
//
// A registered provider/hook closure is stored in a process/thread-local
// registry (`USER_PROVIDERS`, the session-hook table) that OUTLIVES the VM that
// registered it. Its body may call a sibling `pub fn` defined in the SAME
// pipeline module — exactly how Burin wires
// `register_reminder_provider({ evaluate: { ctx -> evaluate_burin_user_reminder_rules(ctx) } })`
// inside `build_loop_options_base`.
//
// Sibling-fn resolution for a module closure goes through the module's function
// registry, which the closure holds only via a `Weak` (`VmClosure::module_functions`).
// The sole strong owner of that registry is the registering VM's `module_cache`.
// When Burin registers the provider during one agent-loop setup and the runtime
// later fires it from a *different* VM (fresh `module_cache`), the original
// registry has been dropped, the `Weak` is dead, and the sibling call falls
// through to host-bridge dispatch — dying with
// `host bridge tool 'evaluate_burin_user_reminder_rules' is not implemented`.
// harn#4113 fixed the *policy* rejection at the same seam; this is the failure
// that "moved" behind it: a name-resolution misdispatch, not a policy trip.
//
// The reproduction registers the closure in a disposable VM, drops that VM
// (releasing its `module_cache`), then fires the provider/hook from a fresh VM
// — the runtime's real invocation path (`evaluate_and_inject`, child VM).

// The module both tests register from: a sibling `pub fn` the registered
// closure calls, plus the `pub fn` that performs the registration.
const REGISTERED_CLOSURE_MODULE: &str = r#"pub fn compute_provider_reminders(ctx) {
  return []
}

pub fn session_hook_decision(payload) {
  return {control: "allow"}
}

pub fn register_provider_closure() {
  register_reminder_provider({
    id: "fn-provider",
    subscribes_to: ["session_start"],
    evaluate: { ctx -> return compute_provider_reminders(ctx) },
  })
  return nil
}

pub fn register_hook_closure() {
  register_session_hook("session_start", { payload ->
    return session_hook_decision(payload)
  })
  return nil
}
"#;

/// Register a provider/hook by loading [`REGISTERED_CLOSURE_MODULE`] into a
/// throwaway VM, invoking `register_fn`, then dropping that VM so its
/// `module_cache` (the only strong owner of the module's function registry) is
/// released. Mirrors Burin registering a provider in one agent-loop VM whose
/// lifetime ends before the provider fires.
async fn register_from_disposable_vm(register_fn: &str) {
    let mut vm = crate::Vm::new();
    crate::stdlib::register_vm_stdlib(&mut vm);
    let exports = vm
        .load_module_exports_from_source(
            "orchestration/tests/registered_closure_module.harn",
            REGISTERED_CLOSURE_MODULE,
        )
        .await
        .expect("compile registered-closure module");
    let register = exports
        .get(register_fn)
        .unwrap_or_else(|| panic!("module must export {register_fn}"))
        .clone();
    vm.call_closure_pub(&register, &[])
        .await
        .expect("registration closure must run");
    // Drop the VM (and `exports`) so the module's function registry is released,
    // leaving the globally-retained closure's `Weak` dangling — the state Burin
    // is in when a later VM fires the provider/hook.
    drop(exports);
    drop(vm);
}

// A registered reminder provider whose `evaluate` closure calls a sibling
// module `pub fn` must still resolve that function when the runtime fires the
// provider from a VM other than the one that registered it. Before the fix the
// dead `Weak` made the call fall through to host-bridge dispatch, dying with
// `host bridge tool 'compute_provider_reminders' is not implemented`.
#[test]
fn registered_provider_closure_resolves_sibling_fn_after_registering_vm_dropped() {
    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                register_from_disposable_vm("register_provider_closure").await;

                // Fire from a fresh VM whose `module_cache` never loaded the
                // module — the runtime's real invocation path.
                let mut vm = crate::Vm::new();
                crate::stdlib::register_vm_stdlib(&mut vm);
                vm.register_async_builtin("__test_fire_reminders", |ctx, _args| async move {
                    let report = crate::llm::reminder_providers::evaluate_and_inject(
                        Some(&ctx),
                        crate::orchestration::HookEvent::SessionStart,
                        "provider-fn-probe",
                        serde_json::json!({}),
                        serde_json::json!({}),
                    )
                    .await?;
                    Ok(crate::json_to_vm_value(&report))
                });
                let chunk = crate::compile_source(
                    r#"pipeline main() {
  const session = agent_session_open("provider-fn-probe")
  agent_session_reset(session)
  __test_fire_reminders()
}"#,
                )?;
                push_execution_policy(CapabilityPolicy {
                    tools: vec!["read".to_string()],
                    ..Default::default()
                });
                let outcome = vm.execute(&chunk).await;
                pop_execution_policy();
                outcome.map(|_| ()).map_err(|error| error.to_string())
            })
            .await
    });

    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();
    result.expect(
        "a registered reminder provider's evaluate closure must resolve its sibling module \
         `pub fn` in-VM even after the registering VM is dropped, not fall through to \
         host-bridge dispatch",
    );
}

// Same invariant for a `register_session_hook` handler closure that calls a
// sibling module `pub fn`, fired from a VM other than the registering one.
#[test]
fn registered_session_hook_closure_resolves_sibling_fn_after_registering_vm_dropped() {
    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                register_from_disposable_vm("register_hook_closure").await;

                let mut vm = crate::Vm::new();
                crate::stdlib::register_vm_stdlib(&mut vm);
                let chunk = crate::compile_source(
                    r#"pipeline main() {
  __host_fire_session_hook("session_start", {session: {id: "hook-fn-probe"}, event: "session_start"})
}"#,
                )?;
                push_execution_policy(CapabilityPolicy {
                    tools: vec!["read".to_string()],
                    ..Default::default()
                });
                let outcome = vm.execute(&chunk).await;
                pop_execution_policy();
                outcome.map(|_| ()).map_err(|error| error.to_string())
            })
            .await
    });

    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();
    result.expect(
        "a registered session hook's handler closure must resolve its sibling module `pub fn` \
         in-VM even after the registering VM is dropped, not fall through to host-bridge \
         dispatch",
    );
}

// Negative control: pinning the module scope must NOT turn every unresolved
// name into an in-VM hit. A provider closure that calls a name which is neither
// a sibling module function nor a builtin must STILL fall through to
// builtin/host-bridge dispatch (here surfacing as "Undefined builtin", the
// no-host equivalent of the host's `-32601 host bridge tool not implemented`).
// This proves the fix narrows retention to the closure's real defining scope
// rather than blanket-swallowing unknown names.
#[test]
fn registered_provider_closure_unknown_name_still_falls_through_to_bridge() {
    const MODULE: &str = r#"pub fn register_unknown_call_provider() {
  register_reminder_provider({
    id: "unknown-call-provider",
    subscribes_to: ["session_start"],
    evaluate: { ctx -> return definitely_not_a_defined_name(ctx) },
  })
  return nil
}
"#;

    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = crate::Vm::new();
                crate::stdlib::register_vm_stdlib(&mut vm);
                let exports = vm
                    .load_module_exports_from_source(
                        "orchestration/tests/unknown_call_module.harn",
                        MODULE,
                    )
                    .await
                    .expect("compile unknown-call module");
                let register = exports
                    .get("register_unknown_call_provider")
                    .expect("module must export register fn")
                    .clone();
                vm.call_closure_pub(&register, &[])
                    .await
                    .expect("registration closure must run");
                drop(exports);
                drop(vm);

                let mut vm = crate::Vm::new();
                crate::stdlib::register_vm_stdlib(&mut vm);
                vm.register_async_builtin("__test_fire_reminders", |ctx, _args| async move {
                    let report = crate::llm::reminder_providers::evaluate_and_inject(
                        Some(&ctx),
                        crate::orchestration::HookEvent::SessionStart,
                        "unknown-call-probe",
                        serde_json::json!({}),
                        serde_json::json!({}),
                    )
                    .await?;
                    Ok(crate::json_to_vm_value(&report))
                });
                let chunk = crate::compile_source(
                    r#"pipeline main() {
  const session = agent_session_open("unknown-call-probe")
  agent_session_reset(session)
  __test_fire_reminders()
}"#,
                )?;
                let outcome = vm.execute(&chunk).await;
                outcome.map(|_| ()).map_err(|error| error.to_string())
            })
            .await
    });

    clear_execution_policy_stacks();
    crate::llm::reminder_providers::clear_reminder_providers();
    clear_session_hooks();
    let error = result
        .expect_err("a genuinely unknown name must not resolve in-VM after the scope-pin fix");
    assert!(
        error.contains("definitely_not_a_defined_name"),
        "the unresolved name must still fall through to builtin/host-bridge dispatch, got: {error}"
    );
}
