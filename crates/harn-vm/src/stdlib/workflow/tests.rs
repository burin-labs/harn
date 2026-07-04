use super::artifact::{load_run_tree, snapshot_trace_spans};
use super::stage::{execute_stage_attempts, replay_stage};
use super::state::{prepare_workflow_state, reset_workflow_run_states, workflow_run_state_count};
use crate::orchestration::{
    save_run_record, stage_verification_contracts, verification_contract_from_verify,
    workflow_verification_contracts, RunChildRecord, RunExecutionRecord, RunRecord, RunStageRecord,
    VerificationContract, WorkflowGraph, WorkflowNode,
};
use crate::tracing::{set_tracing_enabled, span_end, span_start, SpanKind};

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
fn load_run_tree_recovers_child_runs_from_stage_worker_metadata() {
    let dir = std::env::temp_dir().join(format!("harn-run-tree-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).unwrap();
    let child_path = dir.join("child.json");
    let parent_path = dir.join("parent.json");

    let child = RunRecord {
        id: "child".to_string(),
        workflow_id: "wf".to_string(),
        root_run_id: Some("parent".to_string()),
        parent_run_id: Some("parent".to_string()),
        status: "completed".to_string(),
        ..Default::default()
    };
    let parent = RunRecord {
        id: "parent".to_string(),
        workflow_id: "wf".to_string(),
        root_run_id: Some("parent".to_string()),
        status: "completed".to_string(),
        stages: vec![RunStageRecord {
            id: "stage_1".to_string(),
            node_id: "delegate".to_string(),
            metadata: std::collections::BTreeMap::from_iter([(
                "worker".to_string(),
                serde_json::json!({
                    "id": "worker_1",
                    "name": "worker",
                    "task": "delegate",
                    "status": "completed",
                    "child_run_id": "child",
                    "child_run_path": child_path.to_string_lossy(),
                    "snapshot_path": ".harn/workers/worker_1.json",
                }),
            )]),
            ..Default::default()
        }],
        ..Default::default()
    };

    save_run_record(&child, Some(child_path.to_str().unwrap())).unwrap();
    save_run_record(&parent, Some(parent_path.to_str().unwrap())).unwrap();

    let tree = load_run_tree(parent_path.to_str().unwrap()).unwrap();
    assert_eq!(tree["run"]["child_runs"][0]["run_id"], "child");
    assert_eq!(tree["children"][0]["run"]["id"], "child");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deterministic_replay_preserves_worker_child_run_metadata() {
    let child_path = ".harn-runs/child.json";
    let mut stages = std::collections::VecDeque::from(vec![RunStageRecord {
        id: "run:delegate:1".to_string(),
        node_id: "delegate".to_string(),
        kind: "subagent".to_string(),
        status: "completed".to_string(),
        outcome: "subagent_completed".to_string(),
        branch: Some("success".to_string()),
        metadata: std::collections::BTreeMap::from_iter([(
            "worker".to_string(),
            serde_json::json!({
                "id": "worker_1",
                "name": "delegate",
                "task": "delegate task",
                "status": "completed",
                "child_run_id": "child",
                "child_run_path": child_path,
            }),
        )]),
        ..Default::default()
    }]);

    let replayed = replay_stage("delegate", &mut stages).unwrap();
    assert_eq!(replayed.result["worker"]["id"], "worker_1");
    assert_eq!(replayed.result["worker"]["child_run_id"], "child");
    assert_eq!(replayed.result["worker"]["child_run_path"], child_path);
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

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn verify_stage_reads_transcript_from_session_store() {
    crate::reset_thread_local_state();
    let session_id = "session-for-verify-stage".to_string();
    crate::agent_sessions::open_or_create(Some(session_id.clone()));
    for msg in [
        serde_json::json!({"role": "user", "content": "implement the feature"}),
        serde_json::json!({"role": "assistant", "content": "I'll edit the file now."}),
        serde_json::json!({"role": "user", "content": "Tool result: file written"}),
    ] {
        crate::agent_sessions::inject_message(&session_id, crate::stdlib::json_to_vm_value(&msg))
            .expect("inject");
    }

    let mut raw_model_policy = std::collections::BTreeMap::new();
    raw_model_policy.insert(
        "session_id".to_string(),
        crate::value::VmValue::String(arcstr::ArcStr::from(session_id.clone())),
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
        raw_model_policy: Some(crate::value::VmValue::dict(raw_model_policy)),
        ..Default::default()
    };

    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
    let executed = execute_stage_attempts(&ctx, "run tests", "verify", &node, &[], None)
        .await
        .expect("stage executes");

    assert_eq!(executed.status, "completed");
    let transcript = executed
        .transcript
        .expect("verify stage must surface transcript from session");
    let dict = transcript.as_dict().expect("transcript must be a dict");
    let msg_list = match dict.get("messages") {
        Some(crate::value::VmValue::List(list)) => list,
        _ => panic!("transcript must have a messages list"),
    };
    assert_eq!(msg_list.len(), 3);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn command_verify_retry_policy_records_each_attempt() {
    crate::reset_thread_local_state();
    let node = crate::orchestration::WorkflowNode {
        id: Some("verify".to_string()),
        kind: "verify".to_string(),
        retry_policy: crate::orchestration::RetryPolicy {
            max_attempts: 3,
            ..Default::default()
        },
        verify: Some(serde_json::json!({
            "command": "echo nope && exit 7",
            "expect_status": 0,
        })),
        output_contract: crate::orchestration::StageContract {
            output_kinds: vec!["verification_result".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };

    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
    let executed = execute_stage_attempts(&ctx, "run tests", "verify", &node, &[], None)
        .await
        .expect("stage executes");

    assert_eq!(executed.status, "failed");
    assert_eq!(executed.outcome, "verification_failed");
    assert_eq!(executed.branch.as_deref(), Some("failed"));
    assert_eq!(executed.attempts.len(), 3);
    assert!(executed
        .attempts
        .iter()
        .all(|attempt| attempt.status == "failed"));
    assert_eq!(
        executed
            .verification
            .as_ref()
            .and_then(|value| value.get("ok"))
            .and_then(serde_json::Value::as_bool),
        Some(false),
    );
}

/// Retry-with-feedback (design D5): enabling `feedback: true` on a failing
/// stage exercises the embedded loop's retry-task path on attempts 2..N
/// without changing the attempt-recording contract (still one record per
/// attempt, all failed). Proves the feedback branch is live end-to-end through
/// the inverted loop, complementing the `workflow_stage_retry_task` unit
/// conformance that pins the prompt-building semantics.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn command_verify_retry_policy_with_feedback_records_each_attempt() {
    crate::reset_thread_local_state();
    let node = crate::orchestration::WorkflowNode {
        id: Some("verify".to_string()),
        kind: "verify".to_string(),
        retry_policy: crate::orchestration::RetryPolicy {
            max_attempts: 3,
            feedback: Some(crate::orchestration::FeedbackPolicy::Enabled(true)),
            ..Default::default()
        },
        verify: Some(serde_json::json!({
            "command": "echo nope && exit 7",
            "expect_status": 0,
        })),
        output_contract: crate::orchestration::StageContract {
            output_kinds: vec!["verification_result".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };

    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
    let executed = execute_stage_attempts(&ctx, "run tests", "verify", &node, &[], None)
        .await
        .expect("stage executes");

    assert_eq!(executed.status, "failed");
    assert_eq!(executed.outcome, "verification_failed");
    assert_eq!(executed.attempts.len(), 3);
    assert!(executed
        .attempts
        .iter()
        .enumerate()
        .all(|(index, attempt)| attempt.attempt == index + 1 && attempt.status == "failed"));
}

/// Stage-loop inversion pre-work (design D5 step 1): a single
/// `__host_stage_execute_once` round-trip on a static stage must match the
/// legacy `execute_stage_attempts` path's output shape field-for-field.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn host_stage_execute_once_matches_legacy_static_stage_shape() {
    crate::reset_thread_local_state();
    let node = WorkflowNode {
        id: Some("gate".to_string()),
        kind: "join".to_string(),
        retry_policy: crate::orchestration::RetryPolicy {
            max_attempts: 1,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);

    let legacy = execute_stage_attempts(&ctx, "join stage", "gate", &node, &[], None)
        .await
        .expect("legacy path executes");

    let args = vec![
        crate::value::VmValue::String(arcstr::ArcStr::from("gate")),
        super::convert::to_vm(&node).expect("node encodes"),
        crate::value::VmValue::String(arcstr::ArcStr::from("join stage")),
        crate::value::VmValue::Int(1),
        crate::value::VmValue::List(std::sync::Arc::new(Vec::new())),
        crate::value::VmValue::List(std::sync::Arc::new(Vec::new())),
        crate::value::VmValue::Nil,
    ];
    let out = super::host::host_stage_execute_once_builtin(ctx.clone(), args)
        .await
        .expect("builtin executes");
    let dict = out.as_dict().expect("builtin returns a dict");

    assert!(
        matches!(dict.get("ok"), Some(crate::value::VmValue::Bool(true))),
        "builtin must report ok: true"
    );
    assert_eq!(
        dict.get("outcome").map(|value| value.display()),
        Some(legacy.outcome.clone())
    );
    assert_eq!(
        dict.get("branch").map(|value| value.display()),
        legacy.branch.clone()
    );
    assert_eq!(
        crate::llm::vm_value_to_json(dict.get("result").expect("result present")),
        legacy.result
    );
    // A static join stage produces no artifacts / verification and passes
    // the (absent) transcript through in-process.
    assert!(legacy.artifacts.is_empty());
    assert_eq!(
        crate::llm::vm_value_to_json(dict.get("artifacts").expect("artifacts present")),
        serde_json::json!([])
    );
    assert!(
        matches!(dict.get("verification"), Some(crate::value::VmValue::Nil)),
        "static join stage carries no verification"
    );
    assert!(legacy.verification.is_none());
    assert!(
        matches!(dict.get("transcript"), Some(crate::value::VmValue::Nil)),
        "absent transcript passes through as nil"
    );
    assert!(legacy.transcript.is_none());
}

#[test]
fn workflow_verification_contracts_collect_exact_requirements() {
    let graph = WorkflowGraph {
        entry: "act".to_string(),
        nodes: std::collections::BTreeMap::from_iter([(
            "verify".to_string(),
            WorkflowNode {
                id: Some("verify".to_string()),
                kind: "verify".to_string(),
                verify: Some(serde_json::json!({
                    "command": "python verify.py",
                    "expect_status": 0,
                    "required_identifiers": ["rateLimit"],
                    "required_paths": ["src/middleware/rateLimit.ts"],
                    "required_text": ["app.use(rateLimit)"],
                    "notes": ["Do not rename the middleware export."],
                })),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };

    let contracts = workflow_verification_contracts(&graph).expect("verification contracts");
    assert_eq!(contracts.len(), 1);
    assert_eq!(
        contracts[0].required_identifiers,
        vec!["rateLimit".to_string()]
    );
    assert_eq!(
        contracts[0].required_paths,
        vec!["src/middleware/rateLimit.ts".to_string()]
    );
    assert_eq!(
        contracts[0].required_text,
        vec!["app.use(rateLimit)".to_string()]
    );
}

#[test]
fn verification_contract_loads_file_relative_to_execution_context() {
    crate::reset_thread_local_state();
    let temp_dir = std::env::temp_dir().join(format!(
        "harn-verification-contract-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir_all(&temp_dir).expect("temp dir");
    let contract_path = temp_dir.join("verify.contract.json");
    std::fs::write(
        &contract_path,
        serde_json::json!({
            "summary": "Verifier expects the exact middleware symbol.",
            "required_identifiers": ["rateLimit"],
            "required_paths": ["src/middleware/rateLimit.ts"],
            "required_text": ["app.use(rateLimit)"],
        })
        .to_string(),
    )
    .expect("contract file");

    crate::stdlib::process::set_thread_execution_context(Some(RunExecutionRecord {
        cwd: Some(temp_dir.to_string_lossy().into_owned()),
        ..Default::default()
    }));

    let contract = verification_contract_from_verify(
        "act",
        Some(&serde_json::json!({
            "contract_path": "verify.contract.json",
        })),
    )
    .expect("contract loads")
    .expect("contract");

    assert_eq!(contract.source_node.as_deref(), Some("act"));
    assert_eq!(contract.required_identifiers, vec!["rateLimit"]);
    assert_eq!(contract.required_paths, vec!["src/middleware/rateLimit.ts"]);
    assert_eq!(contract.required_text, vec!["app.use(rateLimit)"]);

    crate::reset_thread_local_state();
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[test]
fn stage_verification_contracts_can_scope_to_local_contract_only() {
    crate::reset_thread_local_state();

    let node = WorkflowNode {
        id: Some("act".to_string()),
        kind: "stage".to_string(),
        verify: Some(serde_json::json!({
            "required_paths": ["src/current.ts"],
            "notes": ["Only the current batch path is in scope."],
        })),
        metadata: std::collections::BTreeMap::from_iter([
            (
                crate::orchestration::WORKFLOW_VERIFICATION_SCOPE_METADATA_KEY.to_string(),
                serde_json::json!("local_only"),
            ),
            (
                crate::orchestration::WORKFLOW_VERIFICATION_CONTRACTS_METADATA_KEY.to_string(),
                serde_json::to_value(vec![VerificationContract {
                    source_node: Some("final_verify".to_string()),
                    required_paths: vec!["src/future.ts".to_string()],
                    required_text: vec!["futureOnly".to_string()],
                    ..Default::default()
                }])
                .expect("contract metadata"),
            ),
        ]),
        ..Default::default()
    };
    let contracts = stage_verification_contracts("act", &node).expect("contracts");

    assert_eq!(contracts.len(), 1);
    assert_eq!(contracts[0].required_paths, vec!["src/current.ts"]);
    assert!(contracts[0].required_text.is_empty());
}

/// Regression for harn#2660: an abandoned workflow run leaves its
/// transcript-sized `WorkflowRunState` interned in the thread-local
/// `WORKFLOW_RUN_STATES`. `reset_thread_local_state` (via
/// `reset_stdlib_state`) must drain it so a reused test worker does not
/// accumulate one run per case.
#[test]
fn reset_drains_interned_workflow_run_state() {
    crate::reset_thread_local_state();
    let graph = WorkflowGraph {
        entry: "act".to_string(),
        nodes: std::collections::BTreeMap::from_iter([(
            "act".to_string(),
            WorkflowNode {
                id: Some("act".to_string()),
                kind: "act".to_string(),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let state = prepare_workflow_state(
        "leak-probe".to_string(),
        graph,
        Vec::new(),
        &crate::value::DictMap::new(),
    )
    .expect("prepare workflow state");
    super::state::insert_workflow_state(state);
    assert!(
        workflow_run_state_count() > 0,
        "workflow state should be interned"
    );

    crate::reset_thread_local_state();
    assert_eq!(
        workflow_run_state_count(),
        0,
        "WORKFLOW_RUN_STATES must be empty after reset"
    );

    // Belt-and-suspenders: the direct reset also drains.
    reset_workflow_run_states();
    assert_eq!(workflow_run_state_count(), 0);
}
