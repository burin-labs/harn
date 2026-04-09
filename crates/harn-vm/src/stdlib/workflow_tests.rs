use super::*;
use crate::orchestration::{save_run_record, RunChildRecord, RunRecord};
use crate::tracing::{set_tracing_enabled, span_end, span_start, SpanKind};
use std::cell::Cell;
use std::rc::Rc;

#[test]
fn classify_stage_outcome_fails_when_agent_loop_is_stuck() {
    let (outcome, branch) = classify_stage_outcome(
        "stage",
        &serde_json::json!({"status": "stuck"}),
        &serde_json::json!({"ok": true}),
    );
    assert_eq!(outcome, "stuck");
    assert_eq!(branch.as_deref(), Some("failed"));
}

#[test]
fn classify_stage_outcome_accepts_done_status_for_mutating_stage() {
    let (outcome, branch) = classify_stage_outcome(
        "stage",
        &serde_json::json!({"status": "done"}),
        &serde_json::json!({"ok": true}),
    );
    assert_eq!(outcome, "success");
    assert_eq!(branch.as_deref(), Some("success"));
}

#[test]
fn load_run_tree_recurses_into_child_runs() {
    let dir = std::env::temp_dir().join(format!("harn-run-tree-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let child_path = dir.join("child.json");
    let parent_path = dir.join("parent.json");

    let child = RunRecord {
        id: "child".to_string(),
        workflow_id: "wf".to_string(),
        root_run_id: Some("root".to_string()),
        status: "completed".to_string(),
        ..Default::default()
    };
    let parent = RunRecord {
        id: "parent".to_string(),
        workflow_id: "wf".to_string(),
        root_run_id: Some("root".to_string()),
        status: "completed".to_string(),
        child_runs: vec![RunChildRecord {
            worker_id: "worker_1".to_string(),
            worker_name: "worker".to_string(),
            run_id: Some("child".to_string()),
            run_path: Some(child_path.to_string_lossy().to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };

    save_run_record(&child, Some(child_path.to_str().unwrap())).unwrap();
    save_run_record(&parent, Some(parent_path.to_str().unwrap())).unwrap();

    let tree = load_run_tree(parent_path.to_str().unwrap()).unwrap();
    assert_eq!(tree["run"]["id"], "parent");
    assert_eq!(tree["children"][0]["run"]["id"], "child");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn snapshot_trace_spans_returns_completed_trace_tree() {
    set_tracing_enabled(true);
    let parent = span_start(SpanKind::Pipeline, "workflow".to_string());
    let child = span_start(SpanKind::ToolCall, "read".to_string());
    span_end(child);
    span_end(parent);

    let spans = snapshot_trace_spans();
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].kind, "tool_call");
    assert_eq!(spans[0].parent_id, Some(parent));
    assert_eq!(spans[1].kind, "pipeline");

    set_tracing_enabled(false);
}

#[tokio::test(flavor = "current_thread")]
async fn execute_join_policy_stops_after_first_completion() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let tasks: Vec<LocalTask<i32>> = vec![
                Box::pin(async {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    1
                }),
                Box::pin(async {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    2
                }),
            ];
            let started = std::time::Instant::now();
            let results = execute_join_policy(tasks, "first", None, None).await;
            assert_eq!(results.len(), 1);
            assert!(started.elapsed() < std::time::Duration::from_millis(40));
            assert_eq!(results[0].as_ref().ok().copied(), Some(2));
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn execute_join_policy_honors_quorum_and_concurrency_limit() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let active = Rc::new(Cell::new(0usize));
            let max_seen = Rc::new(Cell::new(0usize));
            let tasks = (0..5)
                .map(|value| {
                    let active = active.clone();
                    let max_seen = max_seen.clone();
                    Box::pin(async move {
                        active.set(active.get() + 1);
                        max_seen.set(max_seen.get().max(active.get()));
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                        active.set(active.get().saturating_sub(1));
                        value
                    }) as LocalTask<i32>
                })
                .collect::<Vec<_>>();
            let results = execute_join_policy(tasks, "quorum", Some(2), Some(2)).await;
            assert_eq!(results.len(), 2);
            assert!(
                max_seen.get() <= 2,
                "observed concurrency {}",
                max_seen.get()
            );
        })
        .await;
}
