use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use harn_vm::event_log::{EventLog, LogEvent, Topic};
use tokio::sync::Mutex;
use tower::util::ServiceExt;

use crate::commands::persona;

use super::dto::{
    PortalLaunchRequest, PortalRunDiff, PortalRunSummary, PortalSpan, PortalStage, PortalStageDebug,
};
use super::launch::{
    build_launch_env, launch_output_logs, materialize_launch_target, prune_completed_launch_jobs,
    scan_launch_targets, validate_launch_request, validated_env_overrides,
};
use super::query::ListRunsQuery;
use super::router::build_router;
use super::run_analysis::{
    build_policy_summary, build_replay_summary, build_run_detail, build_run_summary,
    filter_and_sort_runs, resolve_run_path, scan_runs, summarize_runs,
};
use super::state::PortalState;
use super::transcript::discover_transcript_steps;
use super::util::{date_ms, owning_stage, portal_now_rfc3339, portal_unique_id, preview_text};

fn test_portal_state(run_dir: &Path) -> Arc<PortalState> {
    test_portal_state_with_mutations(run_dir, true)
}

fn test_portal_state_with_mutations(
    run_dir: &Path,
    mutation_endpoints_enabled: bool,
) -> Arc<PortalState> {
    Arc::new(PortalState {
        run_dir: run_dir.to_path_buf(),
        workspace_root: run_dir.to_path_buf(),
        persona_manifest: None,
        persona_state_dir: run_dir.join(".harn/personas"),
        event_log: None,
        launch_program: PathBuf::from("harn"),
        launch_jobs: Arc::new(Mutex::new(HashMap::new())),
        mutation_endpoints_enabled,
    })
}

fn test_portal_state_with_event_log(
    run_dir: &Path,
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
) -> Arc<PortalState> {
    Arc::new(PortalState {
        run_dir: run_dir.to_path_buf(),
        workspace_root: run_dir.to_path_buf(),
        persona_manifest: None,
        persona_state_dir: run_dir.join(".harn/personas"),
        event_log: Some(event_log),
        launch_program: PathBuf::from("harn"),
        launch_jobs: Arc::new(Mutex::new(HashMap::new())),
        mutation_endpoints_enabled: true,
    })
}

fn test_portal_state_with_personas(
    run_dir: &Path,
    manifest: PathBuf,
    persona_state_dir: PathBuf,
) -> Arc<PortalState> {
    Arc::new(PortalState {
        run_dir: run_dir.to_path_buf(),
        workspace_root: run_dir.to_path_buf(),
        persona_manifest: Some(manifest),
        persona_state_dir,
        event_log: None,
        launch_program: PathBuf::from("harn"),
        launch_jobs: Arc::new(Mutex::new(HashMap::new())),
        mutation_endpoints_enabled: true,
    })
}

fn empty_stage_debug() -> PortalStageDebug {
    PortalStageDebug {
        call_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        consumed_artifact_ids: Vec::new(),
        produced_artifact_ids: Vec::new(),
        selected_artifact_ids: Vec::new(),
        worker_id: None,
        error: None,
        model_policy: None,
        auto_compact: None,
        output_visibility: None,
        context_policy: None,
        retry_policy: None,
        capability_policy: None,
        input_contract: None,
        output_contract: None,
        prompt: None,
        system_prompt: None,
        rendered_context: None,
    }
}

fn run_summary_with_duration(id: &str, duration_ms: Option<u64>) -> PortalRunSummary {
    PortalRunSummary {
        path: format!("{id}.json"),
        id: id.to_string(),
        workflow_name: "workflow".to_string(),
        status: "complete".to_string(),
        last_stage_node_id: None,
        failure_summary: None,
        started_at: String::new(),
        finished_at: None,
        duration_ms,
        stage_count: 0,
        child_run_count: 0,
        call_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        models: Vec::new(),
        updated_at_ms: 0,
        skills: Vec::new(),
    }
}

#[test]
fn resolve_run_path_rejects_parent_segments() {
    let temp = tempfile::tempdir().unwrap();
    let error = resolve_run_path(temp.path(), "../outside.json").unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
}

#[test]
fn date_ms_rejects_pre_epoch_dates() {
    assert_eq!(date_ms("1969-12-31T23:59:59Z"), None);
    assert_eq!(date_ms("1970-01-01T00:00:00Z"), Some(0));
}

#[test]
fn portal_launch_ids_are_unique_and_timestamps_are_rfc3339() {
    let mut ids = std::collections::BTreeSet::new();
    for _ in 0..128 {
        let id = portal_unique_id("job");
        assert!(id.starts_with("job-"));
        assert!(ids.insert(id));
    }
    assert!(time::OffsetDateTime::parse(
        &portal_now_rfc3339(),
        &time::format_description::well_known::Rfc3339
    )
    .is_ok());
}

#[test]
fn preview_text_truncates_on_character_boundaries() {
    let line = "é".repeat(181);
    let preview = preview_text(&line);
    assert_eq!(preview, format!("{}...", "é".repeat(180)));
}

#[test]
fn owning_stage_saturates_stage_boundaries() {
    let stages = vec![PortalStage {
        id: "stage-1".to_string(),
        node_id: "huge".to_string(),
        kind: "stage".to_string(),
        status: "running".to_string(),
        outcome: String::new(),
        branch: None,
        started_at: String::new(),
        finished_at: None,
        duration_ms: Some(u64::MAX),
        artifact_count: 0,
        attempt_count: 0,
        verification_summary: None,
        debug: empty_stage_debug(),
    }];
    let span = PortalSpan {
        span_id: 1,
        parent_id: None,
        kind: "llm_call".to_string(),
        name: "call".to_string(),
        start_ms: u64::MAX - 1,
        duration_ms: 1,
        end_ms: u64::MAX,
        label: "call".to_string(),
        lane: 0,
        depth: 0,
        metadata: BTreeMap::new(),
    };

    assert_eq!(
        owning_stage(&span, &stages).map(|stage| stage.node_id.as_str()),
        Some("huge")
    );
}

#[test]
fn summarize_runs_averages_large_durations_without_overflow() {
    let stats = summarize_runs(&[
        run_summary_with_duration("run-1", Some(u64::MAX)),
        run_summary_with_duration("run-2", Some(u64::MAX)),
    ]);

    assert_eq!(stats.avg_duration_ms, u64::MAX);
}

#[test]
fn scan_runs_ignores_non_run_json() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("ignore.json"), "{not valid json").unwrap();
    fs::write(
        temp.path().join("launch.json"),
        serde_json::json!({
            "mode": "playground",
            "task": "hello"
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        temp.path().join("run.json"),
        serde_json::json!({
            "_type": "run_record",
            "id": "run-1",
            "workflow_id": "wf",
            "workflow_name": "demo",
            "task": "task",
            "status": "complete",
            "started_at": "2026-04-03T01:00:00Z",
            "finished_at": "2026-04-03T01:00:02Z",
            "stages": [],
            "transitions": [],
            "checkpoints": [],
            "pending_nodes": [],
            "completed_nodes": [],
            "child_runs": [],
            "artifacts": [],
            "policy": {},
            "metadata": {}
        })
        .to_string(),
    )
    .unwrap();

    let runs = scan_runs(temp.path()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].workflow_name, "demo");
}

#[cfg(unix)]
#[test]
fn scan_runs_does_not_follow_symlinked_directories() {
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(
        outside.path().join("outside.json"),
        serde_json::json!({
            "_type": "run_record",
            "id": "run-outside",
            "workflow_id": "wf",
            "workflow_name": "outside",
            "task": "task",
            "status": "complete",
            "started_at": "2026-04-03T01:00:00Z",
            "finished_at": "2026-04-03T01:00:02Z",
            "stages": [],
            "transitions": [],
            "checkpoints": [],
            "pending_nodes": [],
            "completed_nodes": [],
            "child_runs": [],
            "artifacts": [],
            "policy": {},
            "metadata": {}
        })
        .to_string(),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), temp.path().join("outside-link")).unwrap();

    let runs = scan_runs(temp.path()).unwrap();
    assert!(
        runs.is_empty(),
        "symlinked runs should be ignored: {runs:?}"
    );
}

#[test]
fn build_run_summary_includes_failure_context() {
    let run = harn_vm::orchestration::RunRecord {
        id: "run-1".to_string(),
        workflow_id: "wf".to_string(),
        workflow_name: Some("demo".to_string()),
        status: "failed".to_string(),
        started_at: "2026-04-03T01:00:00Z".to_string(),
        stages: vec![harn_vm::orchestration::RunStageRecord {
            id: "stage-1".to_string(),
            node_id: "verify".to_string(),
            status: "failed".to_string(),
            outcome: "error".to_string(),
            started_at: "2026-04-03T01:00:00Z".to_string(),
            attempts: vec![harn_vm::orchestration::RunStageAttemptRecord {
                error: Some("assertion failed".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };

    let summary = build_run_summary("run.json", 0, &run);
    assert_eq!(summary.last_stage_node_id.as_deref(), Some("verify"));
    assert_eq!(
        summary.failure_summary.as_deref(),
        Some("verify failed: assertion failed")
    );
}

#[test]
fn build_run_detail_exposes_observability_summary() {
    let temp = tempfile::tempdir().unwrap();
    let run_path = temp.path().join("run.json");
    fs::write(&run_path, "{}").unwrap();
    fs::create_dir_all(temp.path().join("run-llm")).unwrap();
    fs::write(temp.path().join("run-llm/llm_transcript.jsonl"), "{}\n").unwrap();

    let run = harn_vm::orchestration::RunRecord {
        id: "run-obs".to_string(),
        workflow_id: "wf".to_string(),
        workflow_name: Some("demo".to_string()),
        task: "task".to_string(),
        status: "failed".to_string(),
        persisted_path: Some(run_path.to_string_lossy().into_owned()),
        stages: vec![harn_vm::orchestration::RunStageRecord {
            id: "stage-1".to_string(),
            node_id: "plan".to_string(),
            kind: "stage".to_string(),
            status: "failed".to_string(),
            outcome: "error".to_string(),
            verification: Some(serde_json::json!({"pass": false})),
            artifacts: vec![harn_vm::orchestration::ArtifactRecord {
                data: Some(serde_json::json!({
                    "trace": {"iterations": 2, "llm_calls": 1, "tool_executions": 1},
                    "task_ledger": {
                        "root_task": "task",
                        "deliverables": [{"id": "deliverable-1", "text": "debug", "status": "open"}],
                        "observations": ["fact one"]
                    }
                })),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };

    let detail = build_run_detail(temp.path(), "run.json", &run);
    assert_eq!(detail.observability.planner_rounds.len(), 1);
    assert_eq!(detail.observability.research_fact_count, 1);
    assert!(detail
        .observability
        .transcript_pointers
        .iter()
        .any(|pointer| pointer.kind == "llm_jsonl"));
}

#[test]
fn build_run_detail_saturates_trace_span_end_times() {
    let temp = tempfile::tempdir().unwrap();
    let run = harn_vm::orchestration::RunRecord {
        id: "run-overflow".to_string(),
        workflow_id: "wf".to_string(),
        workflow_name: Some("demo".to_string()),
        status: "complete".to_string(),
        trace_spans: vec![harn_vm::orchestration::RunTraceSpanRecord {
            span_id: 1,
            kind: "tool_call".to_string(),
            name: "huge-span".to_string(),
            start_ms: u64::MAX - 1,
            duration_ms: 10,
            ..Default::default()
        }],
        ..Default::default()
    };

    let detail = build_run_detail(temp.path(), "run.json", &run);
    assert_eq!(detail.summary.duration_ms, Some(u64::MAX));
    assert_eq!(detail.spans[0].end_ms, u64::MAX);
}

#[test]
fn build_run_detail_joins_tool_call_audit_onto_matching_activity() {
    let temp = tempfile::tempdir().unwrap();
    let run_path = temp.path().join("audit-run.json");
    fs::write(&run_path, "{}").unwrap();

    // Two trace spans, only one of which has a matching audit event.
    let trace_spans = vec![
        harn_vm::orchestration::RunTraceSpanRecord {
            span_id: 1,
            parent_id: None,
            kind: "tool_call".to_string(),
            name: "search_files".to_string(),
            start_ms: 0,
            duration_ms: 12,
            metadata: BTreeMap::from([(
                "call_id".to_string(),
                serde_json::json!("call-with-audit"),
            )]),
            ..Default::default()
        },
        harn_vm::orchestration::RunTraceSpanRecord {
            span_id: 2,
            parent_id: None,
            kind: "tool_call".to_string(),
            name: "read_file".to_string(),
            start_ms: 20,
            duration_ms: 5,
            metadata: BTreeMap::from([(
                "call_id".to_string(),
                serde_json::json!("call-without-audit"),
            )]),
            ..Default::default()
        },
    ];

    let transcript = serde_json::json!({
        "events": [
            {
                "kind": "tool_call_audit",
                "role": "tool",
                "metadata": {
                    "session_id": "s",
                    "tool_call_id": "call-with-audit",
                    "tool_name": "search_files",
                    "audit": {
                        "summary": "Look up rate limiter",
                        "kind": "search",
                        "layers": [
                            {"name": "with_required_reason", "status": "ok"},
                            {"name": "with_consent", "status": "approved", "decided_by": "auto"},
                            {"name": "with_audit_log", "status": "ok"},
                        ],
                        "receipt_uri": "file:///tmp/.harn/receipts/s.jsonl",
                    },
                    "receipt": {
                        "schema_version": 1,
                        "session_id": "s",
                        "tool_call_id": "call-with-audit",
                        "tool_name": "search_files",
                        "iteration": 1,
                        "status": "ok",
                    }
                }
            }
        ]
    });

    let run = harn_vm::orchestration::RunRecord {
        id: "run-audit".to_string(),
        workflow_id: "wf".to_string(),
        workflow_name: Some("audit-demo".to_string()),
        task: "task".to_string(),
        status: "succeeded".to_string(),
        persisted_path: Some(run_path.to_string_lossy().into_owned()),
        trace_spans,
        transcript: Some(transcript),
        ..Default::default()
    };

    let detail = build_run_detail(temp.path(), "audit-run.json", &run);
    let with_audit = detail
        .activities
        .iter()
        .find(|activity| activity.call_id.as_deref() == Some("call-with-audit"))
        .expect("activity for call-with-audit");
    let audit = with_audit
        .audit
        .as_ref()
        .expect("matching activity carries audit");
    assert_eq!(audit.reason.as_deref(), Some("Look up rate limiter"));
    assert_eq!(audit.kind.as_deref(), Some("search"));
    assert_eq!(audit.status, "ok");
    let layer_names: Vec<&str> = audit
        .layers
        .iter()
        .map(|layer| layer.name.as_str())
        .collect();
    assert_eq!(
        layer_names,
        vec!["with_required_reason", "with_consent", "with_audit_log"]
    );
    assert_eq!(
        audit.receipt_uri.as_deref(),
        Some("file:///tmp/.harn/receipts/s.jsonl")
    );

    let without_audit = detail
        .activities
        .iter()
        .find(|activity| activity.call_id.as_deref() == Some("call-without-audit"))
        .expect("activity for call-without-audit");
    assert!(without_audit.audit.is_none(), "no audit event => no chip");
}

#[test]
fn scan_launch_targets_finds_harn_files() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join("examples")).unwrap();
    fs::create_dir_all(temp.path().join("conformance/tests")).unwrap();
    fs::write(temp.path().join("examples/demo.harn"), "pipeline main() {}").unwrap();
    fs::write(
        temp.path().join("conformance/tests/check.harn"),
        "pipeline main() {}",
    )
    .unwrap();

    let targets = scan_launch_targets(temp.path()).unwrap();
    assert_eq!(targets.len(), 2);
    assert!(targets
        .iter()
        .any(|target| target.path == "examples/demo.harn"));
    assert!(targets
        .iter()
        .any(|target| target.path == "conformance/tests/check.harn"));
}

#[test]
fn validate_launch_request_requires_exactly_one_mode() {
    let missing = PortalLaunchRequest {
        file_path: None,
        source: None,
        task: None,
        provider: None,
        model: None,
        env: None,
    };
    assert!(validate_launch_request(&missing).is_err());

    let conflicting = PortalLaunchRequest {
        file_path: Some("examples/demo.harn".to_string()),
        source: Some("pipeline main() {}".to_string()),
        task: None,
        provider: None,
        model: None,
        env: None,
    };
    assert!(validate_launch_request(&conflicting).is_err());
}

#[test]
fn validated_env_overrides_rejects_non_shell_style_names() {
    let env = BTreeMap::from([
        ("OPENAI_API_KEY".to_string(), "secret".to_string()),
        ("bad-key".to_string(), "oops".to_string()),
    ]);
    assert!(validated_env_overrides(Some(&env)).is_err());

    let starts_with_digit = BTreeMap::from([("1BAD".to_string(), "oops".to_string())]);
    assert!(validated_env_overrides(Some(&starts_with_digit)).is_err());
}

#[test]
fn validated_env_overrides_rejects_nul_values() {
    let env = BTreeMap::from([("GOOD_KEY".to_string(), "before\0after".to_string())]);
    assert!(validated_env_overrides(Some(&env)).is_err());
}

#[test]
fn build_launch_env_sets_transcript_dir_inside_workspace() {
    let temp = tempfile::tempdir().unwrap();
    let env = build_launch_env(Some(temp.path()), &BTreeMap::new());
    assert_eq!(
        env.get("HARN_LLM_TRANSCRIPT_DIR").map(String::as_str),
        Some(temp.path().join("run-llm").to_str().unwrap())
    );
}

#[test]
fn launch_output_logs_keeps_only_tail_for_large_outputs() {
    let logs = launch_output_logs("a".repeat(300_000).as_bytes(), b"stderr-tail");
    assert!(logs.starts_with("[truncated to last "));
    assert!(logs.ends_with("stderr-tail"));
    assert!(logs.len() < 270_000);
}

#[test]
fn launch_output_logs_redacts_secrets() {
    let logs = launch_output_logs(
        b"stdout ok",
        b"failed with authorization=Bearer sk-proj-abcdefghijklmnopqrstuvwxyz1234567890",
    );
    assert!(logs.contains("redacted"));
    assert!(!logs.contains("sk-proj-abcdefghijklmnopqrstuvwxyz1234567890"));
}

#[test]
fn prune_completed_launch_jobs_keeps_running_jobs() {
    let mut jobs = HashMap::new();
    for idx in 0..205 {
        jobs.insert(
            format!("job-{idx:03}"),
            super::dto::PortalLaunchJob {
                id: format!("job-{idx:03}"),
                mode: "run".to_string(),
                target_label: "target".to_string(),
                status: "completed".to_string(),
                started_at: format!("started-{idx:03}"),
                finished_at: Some(format!("finished-{idx:03}")),
                exit_code: Some(0),
                logs: String::new(),
                discovered_run_paths: Vec::new(),
                workspace_dir: None,
                transcript_path: None,
            },
        );
    }
    jobs.insert(
        "running".to_string(),
        super::dto::PortalLaunchJob {
            id: "running".to_string(),
            mode: "run".to_string(),
            target_label: "target".to_string(),
            status: "running".to_string(),
            started_at: "started-running".to_string(),
            finished_at: None,
            exit_code: None,
            logs: String::new(),
            discovered_run_paths: Vec::new(),
            workspace_dir: None,
            transcript_path: None,
        },
    );

    prune_completed_launch_jobs(&mut jobs);

    assert_eq!(jobs.len(), 201);
    assert!(jobs.contains_key("running"));
}

#[test]
fn materialize_playground_target_creates_workspace_files() {
    let temp = tempfile::tempdir().unwrap();
    let target = materialize_launch_target(
        temp.path(),
        temp.path(),
        "job-1",
        PortalLaunchRequest {
            file_path: None,
            source: None,
            task: Some("hello world".to_string()),
            provider: Some("mock".to_string()),
            model: Some("mock".to_string()),
            env: None,
        },
    )
    .unwrap();

    let workspace_dir = target.workspace_dir.expect("workspace dir");
    assert!(workspace_dir.join("workflow.harn").exists());
    assert!(workspace_dir.join("task.txt").exists());
    assert!(workspace_dir.join("launch.json").exists());
    let source = fs::read_to_string(workspace_dir.join("workflow.harn")).unwrap();
    assert!(source.contains("workspace_file"));
    assert!(source.contains("persist_path"));
    assert_eq!(source.matches("kind: \"stage\"").count(), 1);
}

#[cfg(unix)]
#[test]
fn materialize_file_target_rejects_symlink_escape() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.harn"), "pipeline main() {}\n").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.harn"), workspace.join("link.harn")).unwrap();

    let error = materialize_launch_target(
        temp.path(),
        &workspace,
        "job-1",
        PortalLaunchRequest {
            file_path: Some("link.harn".to_string()),
            source: None,
            task: None,
            provider: None,
            model: None,
            env: None,
        },
    )
    .unwrap_err();
    assert!(error.contains("stay inside"));
}

#[tokio::test]
async fn api_runs_returns_json() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("run.json"),
        serde_json::json!({
            "_type": "run_record",
            "id": "run-1",
            "workflow_id": "wf",
            "workflow_name": "demo",
            "task": "task",
            "status": "complete",
            "started_at": "2026-04-03T01:00:00Z",
            "finished_at": "2026-04-03T01:00:02Z",
            "stages": [],
            "transitions": [],
            "checkpoints": [],
            "pending_nodes": [],
            "completed_nodes": [],
            "child_runs": [],
            "artifacts": [],
            "policy": {},
            "metadata": {}
        })
        .to_string(),
    )
    .unwrap();

    let app = build_router(test_portal_state(temp.path()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/runs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_personas_exposes_runtime_status() {
    let temp = tempfile::tempdir().unwrap();
    let manifest = temp.path().join("harn.toml");
    fs::write(
        &manifest,
        r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
entry_workflow = "workflows/merge.harn#run"
tools = ["github"]
capabilities = ["git.get_diff"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
triggers = ["github.pr_opened"]
budget = { daily_usd = 1.0, run_usd = 1.0 }
"#,
    )
    .unwrap();
    let state_dir = temp.path().join(".harn/personas");
    persona::pause_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        Some("2026-04-24T12:00:00Z"),
    )
    .await
    .unwrap();

    let app = build_router(test_portal_state_with_personas(
        temp.path(),
        manifest,
        state_dir,
    ));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/persona/status?name=merge_captain&at=2026-04-24T12:00:01Z")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["name"], "merge_captain");
    assert_eq!(payload["state"], "paused");
    assert_eq!(payload["role"], "merge_captain");
    assert_eq!(payload["budget"]["daily_usd"], 1.0);
}

#[tokio::test]
async fn api_trust_graph_returns_records_and_chain() {
    let temp = tempfile::tempdir().unwrap();
    let log = Arc::new(harn_vm::event_log::AnyEventLog::Memory(
        harn_vm::event_log::MemoryEventLog::new(16),
    ));
    harn_vm::append_trust_record(
        &log,
        &harn_vm::TrustRecord::new(
            "agent-a",
            "issue.label",
            None,
            harn_vm::TrustOutcome::Success,
            "trace-a",
            harn_vm::AutonomyTier::Suggest,
        ),
    )
    .await
    .unwrap();

    let app = build_router(test_portal_state_with_event_log(temp.path(), log));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/trust-graph?agent=agent-a&grouped_by_trace=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["records"].as_array().unwrap().len(), 1);
    assert_eq!(payload["chain"]["verified"], true);
    assert_eq!(payload["topics"][0], harn_vm::TRUST_GRAPH_GLOBAL_TOPIC);
}

#[tokio::test]
async fn api_dlq_lists_filters_and_details_entries() {
    let temp = tempfile::tempdir().unwrap();
    let log = Arc::new(harn_vm::event_log::AnyEventLog::Memory(
        harn_vm::event_log::MemoryEventLog::new(16),
    ));
    let topic = Topic::new(harn_vm::TRIGGER_DLQ_TOPIC).unwrap();
    log.append(
        &topic,
        LogEvent::new(
            "dlq_moved",
            serde_json::json!({
                "trigger_id": "cake-classifier",
                "binding_key": "cake-classifier@v1",
                "attempt_count": 2,
                "final_error": "provider returned 503 service unavailable",
                "event": {
                    "id": "trigger_evt_dlq",
                    "provider": "github",
                    "kind": "issues.opened",
                    "headers": {
                        "x-delivery": "delivery-1",
                        "authorization": "Bearer dlq-secret-token"
                    },
                    "provider_payload": {
                        "issue": {"number": 7},
                        "access_token": "payload-secret-token"
                    }
                },
                "attempts": [
                    {
                        "attempt": 1,
                        "completed_at": "2026-04-24T10:00:00Z",
                        "outcome": "failed",
                        "error_msg": "provider returned 500 with sk-proj-abcdefghijklmnopqrstuvwxyz1234567890"
                    }
                ]
            }),
        ),
    )
    .await
    .unwrap();
    let lifecycle_topic = Topic::new(harn_vm::TRIGGERS_LIFECYCLE_TOPIC).unwrap();
    log.append(
        &lifecycle_topic,
        LogEvent::new(
            "predicate.evaluated",
            serde_json::json!({
                "event_id": "trigger_evt_dlq",
                "result": false,
                "reason": "fixture",
                "access_token": "predicate-secret-token"
            }),
        ),
    )
    .await
    .unwrap();

    let app = build_router(test_portal_state_with_event_log(temp.path(), log));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/dlq?error_class=provider_5xx")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["total"], 1);
    assert_eq!(
        payload["entries"][0]["id"],
        "dlq_cake_classifier_v1_trigger_evt_dlq"
    );
    assert_eq!(payload["entries"][0]["error_class"], "provider_5xx");
    assert_eq!(
        payload["entries"][0]["headers"]["authorization"],
        harn_vm::redact::REDACTED_PLACEHOLDER
    );
    assert_eq!(
        payload["entries"][0]["payload"]["access_token"],
        harn_vm::redact::REDACTED_PLACEHOLDER
    );
    let attempt_error = payload["entries"][0]["attempt_history"][0]["error"]
        .as_str()
        .unwrap();
    assert!(attempt_error.contains("redacted"));
    assert!(!attempt_error.contains("sk-proj-abcdefghijklmnopqrstuvwxyz1234567890"));
    assert_eq!(
        payload["entries"][0]["predicate_trace"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        payload["entries"][0]["predicate_trace"][0]["payload"]["access_token"],
        harn_vm::redact::REDACTED_PLACEHOLDER
    );

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/dlq/dlq_cake_classifier_v1_trigger_evt_dlq")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_dlq_purge_writes_tombstone() {
    let temp = tempfile::tempdir().unwrap();
    let log = Arc::new(harn_vm::event_log::AnyEventLog::Memory(
        harn_vm::event_log::MemoryEventLog::new(16),
    ));
    let topic = Topic::new(harn_vm::TRIGGER_DLQ_TOPIC).unwrap();
    log.append(
        &topic,
        LogEvent::new(
            "dlq_moved",
            serde_json::json!({
                "trigger_id": "manual",
                "binding_key": "manual@v1",
                "attempt_count": 1,
                "final_error": "handler VmError::Thrown",
                "event": {
                    "id": "trigger_evt_purge",
                    "provider": "manual",
                    "kind": "manual.fire",
                    "headers": {},
                    "provider_payload": {}
                },
                "attempts": []
            }),
        ),
    )
    .await
    .unwrap();

    let app = build_router(test_portal_state_with_event_log(temp.path(), log));
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/dlq/dlq_manual_v1_trigger_evt_purge/purge")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/dlq")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["total"], 0);
}

#[test]
fn filter_and_sort_runs_applies_search_status_and_ordering() {
    let runs = vec![
        PortalRunSummary {
            path: "alpha.json".to_string(),
            id: "run-alpha".to_string(),
            workflow_name: "alpha".to_string(),
            status: "completed".to_string(),
            last_stage_node_id: Some("finalize".to_string()),
            failure_summary: None,
            started_at: "2026-04-04T10:00:00Z".to_string(),
            finished_at: None,
            duration_ms: Some(100),
            stage_count: 1,
            child_run_count: 0,
            call_count: 1,
            input_tokens: 10,
            output_tokens: 5,
            models: vec!["gpt-4o".to_string()],
            updated_at_ms: 1,
            skills: Vec::new(),
        },
        PortalRunSummary {
            path: "beta.json".to_string(),
            id: "run-beta".to_string(),
            workflow_name: "beta".to_string(),
            status: "failed".to_string(),
            last_stage_node_id: Some("verify".to_string()),
            failure_summary: Some("assertion failed".to_string()),
            started_at: "2026-04-04T11:00:00Z".to_string(),
            finished_at: None,
            duration_ms: Some(200),
            stage_count: 2,
            child_run_count: 0,
            call_count: 2,
            input_tokens: 20,
            output_tokens: 10,
            models: vec!["qwen".to_string()],
            updated_at_ms: 2,
            skills: Vec::new(),
        },
    ];

    let query = ListRunsQuery {
        q: Some("ASSERTION".to_string()),
        workflow: None,
        status: Some("failed".to_string()),
        sort: Some("duration".to_string()),
        page: Some(1),
        page_size: Some(25),
        skill: None,
    };

    let filtered = filter_and_sort_runs(runs, &query);
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].path, "beta.json");
}

#[tokio::test]
async fn api_meta_returns_workspace_and_run_dir() {
    let temp = tempfile::tempdir().unwrap();
    let app = build_router(test_portal_state(temp.path()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/meta")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_highlight_keywords_returns_payload() {
    let temp = tempfile::tempdir().unwrap();
    let app = build_router(test_portal_state(temp.path()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/highlight/keywords")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_llm_options_returns_payload() {
    let temp = tempfile::tempdir().unwrap();
    let app = build_router(test_portal_state(temp.path()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/llm/options")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn remote_portal_state_rejects_launch_mutation_endpoint() {
    let temp = tempfile::tempdir().unwrap();
    let app = build_router(test_portal_state_with_mutations(temp.path(), false));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/launch")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"source":"pipeline test(task) {}"}"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn portal_index_and_assets_are_served() {
    let temp = tempfile::tempdir().unwrap();
    let app = build_router(test_portal_state(temp.path()));

    let index_response = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(index_response.status(), StatusCode::OK);

    let asset_response = app
        .oneshot(
            Request::builder()
                .uri("/assets/portal/app.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(asset_response.status(), StatusCode::OK);
}

#[tokio::test]
async fn portal_assets_reject_traversal_segments() {
    let temp = tempfile::tempdir().unwrap();
    let app = build_router(test_portal_state(temp.path()));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/assets/../index.html")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_run_rejects_escaping_paths() {
    let temp = tempfile::tempdir().unwrap();
    let app = build_router(test_portal_state(temp.path()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/run?path=../outside.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn api_run_returns_not_found_for_missing_runs() {
    let temp = tempfile::tempdir().unwrap();
    let app = build_router(test_portal_state(temp.path()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/run?path=missing.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_compare_returns_stage_diffs() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("left.json"),
        serde_json::json!({
            "_type": "run_record",
            "id": "run-left",
            "workflow_id": "wf",
            "workflow_name": "demo",
            "task": "task",
            "status": "completed",
            "started_at": "2026-04-03T01:00:00Z",
            "finished_at": "2026-04-03T01:00:02Z",
            "stages": [{
                "id": "stage-1",
                "node_id": "plan",
                "status": "completed",
                "outcome": "success",
                "started_at": "2026-04-03T01:00:00Z",
                "finished_at": "2026-04-03T01:00:01Z",
                "artifacts": []
            }],
            "transitions": [],
            "checkpoints": [],
            "pending_nodes": [],
            "completed_nodes": ["plan"],
            "child_runs": [],
            "artifacts": [],
            "policy": {},
            "metadata": {}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        temp.path().join("right.json"),
        serde_json::json!({
            "_type": "run_record",
            "id": "run-right",
            "workflow_id": "wf",
            "workflow_name": "demo",
            "task": "task",
            "status": "failed",
            "started_at": "2026-04-03T01:01:00Z",
            "finished_at": "2026-04-03T01:01:03Z",
            "stages": [{
                "id": "stage-1",
                "node_id": "plan",
                "status": "failed",
                "outcome": "error",
                "started_at": "2026-04-03T01:01:00Z",
                "finished_at": "2026-04-03T01:01:02Z",
                "artifacts": [{"id":"artifact-1","kind":"artifact","created_at":"2026-04-03T01:01:02Z"}]
            }],
            "transitions": [{"id":"transition-1","to_node_id":"plan","timestamp":"2026-04-03T01:01:02Z"}],
            "checkpoints": [{"id":"checkpoint-1","reason":"error","persisted_at":"2026-04-03T01:01:02Z"}],
            "pending_nodes": [],
            "completed_nodes": [],
            "child_runs": [],
            "artifacts": [{"id":"artifact-1","kind":"artifact","created_at":"2026-04-03T01:01:02Z"}],
            "policy": {},
            "metadata": {}
        })
        .to_string(),
    )
    .unwrap();

    let app = build_router(test_portal_state(temp.path()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/compare?left=left.json&right=right.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let diff: PortalRunDiff = serde_json::from_slice(&body).unwrap();
    assert!(diff.status_changed);
    assert_eq!(diff.left_status, "completed");
    assert_eq!(diff.right_status, "failed");
    assert!(!diff.stage_diffs.is_empty());
    assert!(diff.tool_diffs.is_empty());
    assert!(!diff.observability_diffs.is_empty());
    assert_eq!(diff.transition_count_delta, 1);
    assert_eq!(diff.artifact_count_delta, 1);
    assert_eq!(diff.checkpoint_count_delta, 1);
}

#[tokio::test]
async fn api_compare_rejects_escaping_paths() {
    let temp = tempfile::tempdir().unwrap();
    let app = build_router(test_portal_state(temp.path()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/compare?left=../left.json&right=right.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn api_compare_returns_not_found_for_missing_runs() {
    let temp = tempfile::tempdir().unwrap();
    let app = build_router(test_portal_state(temp.path()));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/compare?left=left.json&right=right.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[test]
fn discover_transcript_steps_reads_sibling_sidecar() {
    let temp = tempfile::tempdir().unwrap();
    let run_path = temp.path().join("run.json");
    fs::write(&run_path, "{}").unwrap();
    let llm_dir = temp.path().join("run-llm");
    fs::create_dir_all(&llm_dir).unwrap();
    // Event-stream shape: system_prompt + tool_schemas once, then a
    // user message, then provider_call_request / response. Parser
    // reconstructs a PortalTranscriptStep by replaying events.
    fs::write(
        llm_dir.join("llm_transcript.jsonl"),
        concat!(
            "{\"type\":\"system_prompt\",\"content\":\"Be helpful\",\"hash\":1}\n",
            "{\"type\":\"tool_schemas\",\"schemas\":[{\"name\":\"read\"}],\"hash\":2}\n",
            "{\"type\":\"message\",\"role\":\"user\",\"content\":\"Do X\",\"iteration\":1}\n",
            "{\"type\":\"provider_call_request\",\"call_id\":\"call-1\",\"iteration\":1,\"model\":\"mock\"}\n",
            "{\"type\":\"provider_call_response\",\"call_id\":\"call-1\",\"iteration\":1,\"model\":\"mock\",\"text\":\"Done\",\"input_tokens\":10,\"output_tokens\":4,\"tool_calls\":[{\"name\":\"read\"}]}\n"
        ),
    )
    .unwrap();

    let steps = discover_transcript_steps(temp.path(), "run.json").unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].tool_calls, vec!["read".to_string()]);
    assert_eq!(steps[0].added_messages, 1);
    assert_eq!(steps[0].response_text.as_deref(), Some("Done"));
    assert_eq!(steps[0].system_prompt.as_deref(), Some("Be helpful"));
}

#[test]
fn build_policy_summary_reads_validation_metadata() {
    let run = harn_vm::orchestration::RunRecord {
        policy: harn_vm::orchestration::CapabilityPolicy {
            tools: vec!["read".to_string(), "exec".to_string()],
            tools_restricted: false,
            capabilities: BTreeMap::from([(
                "workspace".to_string(),
                vec!["read_text".to_string(), "list".to_string()],
            )]),
            capabilities_restricted: false,
            workspace_roots: vec!["/tmp/project".to_string()],
            read_only_roots: Vec::new(),
            side_effect_level: Some("workspace_write".to_string()),
            recursion_limit: Some(4),
            tool_arg_constraints: vec![harn_vm::orchestration::ToolArgConstraint {
                tool: "read".to_string(),
                arg_patterns: vec!["src/*".to_string()],
                arg_key: Some("path".to_string()),
            }],
            tool_annotations: BTreeMap::new(),
            sandbox_profile: harn_vm::orchestration::SandboxProfile::default(),
            process_sandbox: Default::default(),
        },
        metadata: BTreeMap::from([(
            "validation".to_string(),
            serde_json::json!({
                "valid": false,
                "errors": ["missing edge"],
                "warnings": ["unused node"],
                "reachable_nodes": ["plan"],
            }),
        )]),
        ..Default::default()
    };

    let summary = build_policy_summary(&run);

    assert_eq!(summary.tools, vec!["read".to_string(), "exec".to_string()]);
    assert!(summary
        .capabilities
        .contains(&"workspace.read_text".to_string()));
    assert_eq!(summary.validation_valid, Some(false));
    assert_eq!(summary.validation_errors, vec!["missing edge".to_string()]);
    assert_eq!(summary.validation_warnings, vec!["unused node".to_string()]);
    assert_eq!(summary.reachable_nodes, vec!["plan".to_string()]);
}

#[test]
fn build_replay_summary_reads_fixture_metadata() {
    let fixture = harn_vm::orchestration::ReplayFixture {
        id: "fixture-1".to_string(),
        source_run_id: "run-1".to_string(),
        created_at: "2026-04-04T00:00:00Z".to_string(),
        expected_status: "completed".to_string(),
        stage_assertions: vec![harn_vm::orchestration::ReplayStageAssertion {
            node_id: "plan".to_string(),
            expected_status: "completed".to_string(),
            expected_outcome: "success".to_string(),
            expected_branch: Some("true".to_string()),
            required_artifact_kinds: vec!["notes".to_string()],
            visible_text_contains: Some("done".to_string()),
        }],
        ..Default::default()
    };

    let summary = build_replay_summary(Some(&fixture)).unwrap();
    assert_eq!(summary.fixture_id, "fixture-1");
    assert_eq!(summary.stage_assertions.len(), 1);
    assert_eq!(summary.stage_assertions[0].node_id, "plan");
}
