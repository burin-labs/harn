use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::sync::Mutex;
use tower::util::ServiceExt;

use super::dto::{PortalLaunchRequest, PortalRunDiff, PortalRunSummary};
use super::launch::{
    build_launch_env, materialize_launch_target, scan_launch_targets, validate_launch_request,
    validated_env_overrides,
};
use super::query::ListRunsQuery;
use super::router::build_router;
use super::run_analysis::{
    build_policy_summary, build_replay_summary, build_run_detail, build_run_summary,
    filter_and_sort_runs, resolve_run_path, scan_runs,
};
use super::state::PortalState;
use super::transcript::discover_transcript_steps;

fn test_portal_state(run_dir: &Path) -> Arc<PortalState> {
    Arc::new(PortalState {
        run_dir: run_dir.to_path_buf(),
        workspace_root: run_dir.to_path_buf(),
        event_log: None,
        launch_program: PathBuf::from("harn"),
        launch_jobs: Arc::new(Mutex::new(HashMap::new())),
    })
}

#[test]
fn resolve_run_path_rejects_parent_segments() {
    let temp = tempfile::tempdir().unwrap();
    let error = resolve_run_path(temp.path(), "../outside.json").unwrap_err();
    assert_eq!(error.0, StatusCode::BAD_REQUEST);
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
    fs::write(temp.path().join("run-llm/llm_transcript.jsonl"), "").unwrap();

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
        q: Some("assertion".to_string()),
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
            capabilities: BTreeMap::from([(
                "workspace".to_string(),
                vec!["read_text".to_string(), "list".to_string()],
            )]),
            workspace_roots: vec!["/tmp/project".to_string()],
            side_effect_level: Some("workspace_write".to_string()),
            recursion_limit: Some(4),
            tool_arg_constraints: vec![harn_vm::orchestration::ToolArgConstraint {
                tool: "read".to_string(),
                arg_patterns: vec!["src/*".to_string()],
                arg_key: Some("path".to_string()),
            }],
            tool_annotations: BTreeMap::new(),
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
