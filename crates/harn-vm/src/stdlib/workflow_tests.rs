use super::*;
use crate::orchestration::{render_artifacts_context, render_workflow_prompt};
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
fn classify_stage_outcome_fails_when_required_write_never_succeeds() {
    let (outcome, branch) = classify_stage_outcome(
        "stage",
        &serde_json::json!({"status": "failed"}),
        &serde_json::json!({"ok": true}),
    );
    assert_eq!(outcome, "failed");
    assert_eq!(branch.as_deref(), Some("failed"));
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
            run_path: Some(child_path.to_string_lossy().into_owned()),
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

#[test]
fn render_workflow_prompt_puts_task_before_context() {
    let prompt = render_workflow_prompt(
        "Create the missing test file with one edit call.",
        Some("Create Required Outputs"),
        "<artifact>\n<title>tests/unit/test_example.py</title>\n<body>\npass\n</body>\n</artifact>",
    );
    let task_index = prompt
        .find("<workflow_task>")
        .expect("workflow task block should exist");
    let context_index = prompt
        .find("<workflow_context>")
        .expect("workflow context block should exist");
    assert!(
        task_index < context_index,
        "task block should precede context block"
    );
    assert!(prompt.contains("<label>Create Required Outputs</label>"));
    assert!(prompt.contains("Create the missing test file with one edit call."));
    assert!(prompt.contains("<workflow_response_contract>"));
    assert!(
        prompt.trim_end().ends_with("</workflow_response_contract>"),
        "prompt should end on the response contract instead of artifact text"
    );
}

#[test]
fn render_artifacts_context_uses_structured_artifact_blocks() {
    let artifacts = vec![crate::orchestration::ArtifactRecord {
        kind: "workspace_file".to_string(),
        title: Some("tests/unit/test_example.py".to_string()),
        text: Some("def test_example():\n    assert True\n".to_string()),
        source: Some("required_output_phase".to_string()),
        freshness: Some("fresh".to_string()),
        priority: Some(70),
        ..Default::default()
    }];
    let rendered =
        render_artifacts_context(&artifacts, &crate::orchestration::ContextPolicy::default());
    assert!(rendered.contains("<artifact>"));
    assert!(rendered.contains("<title>tests/unit/test_example.py</title>"));
    assert!(rendered.contains("<body>\ndef test_example():"));
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

#[tokio::test(flavor = "current_thread")]
async fn failed_verify_stage_preserves_verification_artifact_and_result() {
    let node = crate::orchestration::WorkflowNode {
        id: Some("verify".to_string()),
        kind: "verify".to_string(),
        retry_policy: crate::orchestration::RetryPolicy {
            max_attempts: 1,
            ..Default::default()
        },
        verify: Some(serde_json::json!({
            "command": "printf nope; exit 1",
            "expect_status": 0,
        })),
        output_contract: crate::orchestration::StageContract {
            output_kinds: vec!["verification_result".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };

    let executed = execute_stage_attempts("run verification", "verify", &node, &[], None)
        .await
        .expect("stage executes");

    assert_eq!(executed.status, "failed");
    assert_eq!(executed.outcome, "verification_failed");
    assert_eq!(executed.branch.as_deref(), Some("failed"));
    assert_eq!(executed.artifacts.len(), 1);
    assert_eq!(executed.artifacts[0].kind, "verification_result");
    assert!(executed.result["visible_text"]
        .as_str()
        .unwrap_or("")
        .contains("nope"));
    assert_eq!(
        executed
            .verification
            .as_ref()
            .and_then(|value| value.get("ok"))
            .and_then(|value| value.as_bool()),
        Some(false)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn verify_stage_preserves_input_transcript() {
    let messages = vec![
        serde_json::json!({"role": "user", "content": "implement the feature"}),
        serde_json::json!({"role": "assistant", "content": "I'll edit the file now."}),
        serde_json::json!({"role": "user", "content": "Tool result: file written"}),
    ];
    let input_transcript = crate::llm::helpers::transcript_to_vm_with_events(
        Some("test-transcript-id".to_string()),
        None,
        None,
        &messages,
        Vec::new(),
        Vec::new(),
        Some("active"),
    );

    let node = crate::orchestration::WorkflowNode {
        id: Some("verify".to_string()),
        kind: "verify".to_string(),
        retry_policy: crate::orchestration::RetryPolicy {
            max_attempts: 1,
            ..Default::default()
        },
        verify: Some(serde_json::json!({
            "command": "echo ok",
            "expect_status": 0,
        })),
        output_contract: crate::orchestration::StageContract {
            output_kinds: vec!["verification_result".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };

    let executed =
        execute_stage_attempts("run tests", "verify", &node, &[], Some(input_transcript))
            .await
            .expect("stage executes");

    assert_eq!(executed.status, "completed");
    let transcript = executed
        .transcript
        .expect("verify stage must preserve input transcript");
    let dict = transcript.as_dict().expect("transcript must be a dict");
    let msg_list = match dict.get("messages") {
        Some(crate::value::VmValue::List(list)) => list,
        _ => panic!("transcript must have a messages list"),
    };
    assert_eq!(
        msg_list.len(),
        3,
        "verify stage should preserve all 3 messages from the implement stage transcript"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn verify_stage_with_reset_transcript_policy_clears_transcript() {
    let messages = vec![
        serde_json::json!({"role": "user", "content": "implement the feature"}),
        serde_json::json!({"role": "assistant", "content": "Done."}),
    ];
    let input_transcript = crate::llm::helpers::transcript_to_vm_with_events(
        Some("test-transcript-id".to_string()),
        None,
        None,
        &messages,
        Vec::new(),
        Vec::new(),
        Some("active"),
    );

    let node = crate::orchestration::WorkflowNode {
        id: Some("verify".to_string()),
        kind: "verify".to_string(),
        retry_policy: crate::orchestration::RetryPolicy {
            max_attempts: 1,
            ..Default::default()
        },
        transcript_policy: crate::orchestration::TranscriptPolicy {
            mode: Some("reset".to_string()),
            ..Default::default()
        },
        verify: Some(serde_json::json!({
            "command": "echo ok",
            "expect_status": 0,
        })),
        output_contract: crate::orchestration::StageContract {
            output_kinds: vec!["verification_result".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };

    let executed =
        execute_stage_attempts("run tests", "verify", &node, &[], Some(input_transcript))
            .await
            .expect("stage executes");

    assert!(
        executed.transcript.is_none(),
        "reset transcript policy should clear the transcript even for verify stages"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn failing_stage_records_exactly_one_attempt_regardless_of_max_attempts() {
    // `retry_policy.max_attempts` is a no-op. A stage that fails runs once;
    // iteration lives at the workflow-graph level.
    let node = crate::orchestration::WorkflowNode {
        id: Some("verify".to_string()),
        kind: "verify".to_string(),
        retry_policy: crate::orchestration::RetryPolicy {
            max_attempts: 5,
            backoff_ms: Some(1),
            ..Default::default()
        },
        verify: Some(serde_json::json!({
            "command": "exit 7",
            "expect_status": 0,
        })),
        output_contract: crate::orchestration::StageContract {
            output_kinds: vec!["verification_result".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };

    let executed = execute_stage_attempts("verify", "verify", &node, &[], None)
        .await
        .expect("stage executes");

    assert_eq!(
        executed.attempts.len(),
        1,
        "failing stage must record exactly one attempt; retry-loop is removed"
    );
    assert_eq!(executed.status, "failed");
    assert_eq!(executed.branch.as_deref(), Some("failed"));
}

#[tokio::test(flavor = "current_thread")]
async fn succeeding_stage_records_single_attempt() {
    let node = crate::orchestration::WorkflowNode {
        id: Some("verify".to_string()),
        kind: "verify".to_string(),
        retry_policy: crate::orchestration::RetryPolicy {
            max_attempts: 3,
            ..Default::default()
        },
        verify: Some(serde_json::json!({
            "command": "echo ok",
            "expect_status": 0,
        })),
        output_contract: crate::orchestration::StageContract {
            output_kinds: vec!["verification_result".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };

    let executed = execute_stage_attempts("verify", "verify", &node, &[], None)
        .await
        .expect("stage executes");

    assert_eq!(executed.attempts.len(), 1);
    assert_eq!(executed.status, "completed");
}

#[tokio::test(flavor = "current_thread")]
async fn stage_task_reaches_execution_verbatim() {
    let node = crate::orchestration::WorkflowNode {
        id: Some("verify".to_string()),
        kind: "verify".to_string(),
        retry_policy: crate::orchestration::RetryPolicy {
            max_attempts: 3,
            ..Default::default()
        },
        verify: Some(serde_json::json!({
            "command": "echo 'verification'; exit 1",
            "expect_status": 0,
        })),
        output_contract: crate::orchestration::StageContract {
            output_kinds: vec!["verification_result".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };

    let executed =
        execute_stage_attempts("the original task, pristine", "verify", &node, &[], None)
            .await
            .expect("stage executes");

    assert_eq!(executed.attempts.len(), 1);
}
