use super::artifact::{load_run_tree, snapshot_trace_spans};
use super::map::{execute_join_policy, LocalTask};
use super::register::execute_workflow;
use super::stage::{classify_stage_outcome, execute_stage_attempts, replay_stage};
use crate::orchestration::{
    inject_workflow_verification_contracts, render_artifacts_context, render_workflow_prompt,
    save_run_record, workflow_verification_contracts, RunChildRecord, RunExecutionRecord,
    RunRecord, RunStageRecord, VerificationContract, VerificationRequirement, WorkflowEdge,
    WorkflowGraph, WorkflowNode,
};
use crate::tracing::{set_tracing_enabled, span_end, span_start, SpanKind};
use crate::value::VmValue;
use std::cell::Cell;
use std::collections::BTreeMap;
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
            metadata: BTreeMap::from([(
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
        metadata: BTreeMap::from([(
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

#[test]
fn render_workflow_prompt_puts_task_before_context() {
    let prompt = render_workflow_prompt(
        "Create the missing test file with one edit call.",
        Some("Create Required Outputs"),
        "",
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
fn render_workflow_prompt_places_verification_before_context() {
    let prompt = render_workflow_prompt(
        "Implement the verifier-exact wiring.",
        Some("Implement"),
        "<contract>\n<required_identifiers>\n- rateLimit\n</required_identifiers>\n</contract>",
        "<artifact>\n<title>src/server.ts</title>\n<body>\nexisting code\n</body>\n</artifact>",
    );

    let verification_index = prompt
        .find("<workflow_verification>")
        .expect("verification block should exist");
    let context_index = prompt
        .find("<workflow_context>")
        .expect("context block should exist");
    assert!(
        verification_index < context_index,
        "verification block should precede artifact context"
    );
    assert!(prompt.contains("rateLimit"));
}

#[test]
fn render_workflow_prompt_makes_current_stage_scope_authoritative() {
    let prompt = render_workflow_prompt(
        "Only update src/current.ts.",
        Some("Execute Current Batch"),
        "",
        "<artifact>\n<title>Action graph</title>\n<body>\nFuture step: run final verification\n</body>\n</artifact>",
    );

    assert!(prompt.contains("Treat `<workflow_context>` as supporting evidence"));
    assert!(prompt.contains("do only what the current workflow task and system prompt authorize"));
    assert!(prompt.contains("When the current stage is complete, stop"));
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
            "command": failing_verify_command(),
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

fn failing_verify_command() -> &'static str {
    if cfg!(target_os = "windows") {
        "echo nope && exit /b 1"
    } else {
        "printf nope; exit 1"
    }
}

#[tokio::test(flavor = "current_thread")]
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
        crate::value::VmValue::String(std::rc::Rc::from(session_id.clone())),
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
        raw_model_policy: Some(crate::value::VmValue::Dict(std::rc::Rc::new(
            raw_model_policy,
        ))),
        ..Default::default()
    };

    let executed = execute_stage_attempts("run tests", "verify", &node, &[], None)
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

#[test]
fn workflow_verification_contracts_collect_exact_requirements() {
    let graph = WorkflowGraph {
        entry: "act".to_string(),
        nodes: BTreeMap::from([(
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

#[tokio::test(flavor = "current_thread")]
async fn workflow_execute_injects_verify_contract_into_act_prompt() {
    crate::reset_thread_local_state();
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: "done".to_string(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: false,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock-model".to_string(),
        provider: Some("mock".to_string()),
        blocks: None,
        error: None,
    });

    let temp_dir = std::env::temp_dir().join(format!("harn-issue-126-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&temp_dir).expect("temp dir");
    let persist_path = temp_dir.join("run.json");

    let graph = WorkflowGraph {
        type_name: "workflow_graph".to_string(),
        id: "wf".to_string(),
        entry: "act".to_string(),
        nodes: BTreeMap::from([
            (
                "act".to_string(),
                WorkflowNode {
                    id: Some("act".to_string()),
                    kind: "stage".to_string(),
                    mode: Some("llm".to_string()),
                    model_policy: crate::orchestration::ModelPolicy {
                        provider: Some("mock".to_string()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ),
            (
                "verify".to_string(),
                WorkflowNode {
                    id: Some("verify".to_string()),
                    kind: "verify".to_string(),
                    verify: Some(serde_json::json!({
                        "command": "echo ok",
                        "expect_status": 0,
                        "required_identifiers": ["rateLimit"],
                        "required_text": ["app.use(rateLimit)"],
                    })),
                    output_contract: crate::orchestration::StageContract {
                        output_kinds: vec!["verification_result".to_string()],
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ),
        ]),
        edges: vec![WorkflowEdge {
            from: "act".to_string(),
            to: "verify".to_string(),
            branch: None,
            label: None,
        }],
        ..Default::default()
    };

    let result = execute_workflow(
        "Implement the verifier-exact middleware.".to_string(),
        graph,
        Vec::new(),
        BTreeMap::from([
            (
                "persist_path".to_string(),
                crate::value::VmValue::String(Rc::from(
                    persist_path.to_string_lossy().into_owned(),
                )),
            ),
            ("max_steps".to_string(), crate::value::VmValue::Int(2)),
        ]),
    )
    .await
    .expect("workflow executes");

    let run_value = result
        .as_dict()
        .and_then(|value| value.get("run"))
        .cloned()
        .expect("workflow envelope run");
    let run = crate::orchestration::normalize_run_record(&run_value).expect("run record");
    let act_stage = run
        .stages
        .iter()
        .find(|stage| stage.node_id == "act")
        .expect("act stage");
    let prompt = act_stage
        .metadata
        .get("prompt")
        .and_then(|value| value.as_str())
        .expect("prompt metadata");
    assert!(prompt.contains("<workflow_verification>"));
    assert!(prompt.contains("rateLimit"));
    assert!(prompt.contains("app.use(rateLimit)"));

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test(flavor = "current_thread")]
async fn stage_prompt_loads_contract_file_relative_to_execution_context() {
    crate::reset_thread_local_state();
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: "done".to_string(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: false,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock-model".to_string(),
        provider: Some("mock".to_string()),
        blocks: None,
        error: None,
    });

    let temp_dir =
        std::env::temp_dir().join(format!("harn-issue-126-file-{}", uuid::Uuid::now_v7()));
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

    let mut node = WorkflowNode {
        id: Some("act".to_string()),
        kind: "stage".to_string(),
        mode: Some("llm".to_string()),
        model_policy: crate::orchestration::ModelPolicy {
            provider: Some("mock".to_string()),
            ..Default::default()
        },
        verify: Some(serde_json::json!({
            "contract_path": "verify.contract.json",
        })),
        ..Default::default()
    };
    inject_workflow_verification_contracts(
        &mut node,
        &[VerificationContract {
            source_node: Some("verify".to_string()),
            checks: vec![VerificationRequirement {
                kind: "identifier".to_string(),
                value: "rateLimit".to_string(),
                note: Some("Use the exact exported name.".to_string()),
            }],
            ..Default::default()
        }],
    );

    let executed = execute_stage_attempts("Implement the middleware.", "act", &node, &[], None)
        .await
        .expect("stage executes");
    let prompt = executed
        .result
        .get("prompt")
        .and_then(|value| value.as_str())
        .expect("prompt");
    assert!(prompt.contains("rateLimit"));
    assert!(prompt.contains("src/middleware/rateLimit.ts"));
    assert!(prompt.contains("app.use(rateLimit)"));

    crate::reset_thread_local_state();
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test(flavor = "current_thread")]
async fn stage_prompt_can_scope_verification_to_local_contract_only() {
    crate::reset_thread_local_state();
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: "done".to_string(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: false,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock-model".to_string(),
        provider: Some("mock".to_string()),
        blocks: None,
        error: None,
    });

    let mut node = WorkflowNode {
        id: Some("act".to_string()),
        kind: "stage".to_string(),
        mode: Some("llm".to_string()),
        model_policy: crate::orchestration::ModelPolicy {
            provider: Some("mock".to_string()),
            ..Default::default()
        },
        verify: Some(serde_json::json!({
            "required_paths": ["src/current.ts"],
            "notes": ["Only the current batch path is in scope."],
        })),
        metadata: BTreeMap::from([(
            crate::orchestration::WORKFLOW_VERIFICATION_SCOPE_METADATA_KEY.to_string(),
            serde_json::json!("local_only"),
        )]),
        ..Default::default()
    };
    inject_workflow_verification_contracts(
        &mut node,
        &[VerificationContract {
            source_node: Some("final_verify".to_string()),
            required_paths: vec!["src/future.ts".to_string()],
            required_text: vec!["futureOnly".to_string()],
            ..Default::default()
        }],
    );

    let executed = execute_stage_attempts("Only update src/current.ts.", "act", &node, &[], None)
        .await
        .expect("stage executes");
    let prompt = executed
        .result
        .get("prompt")
        .and_then(|value| value.as_str())
        .expect("prompt");
    assert!(prompt.contains("src/current.ts"));
    assert!(!prompt.contains("src/future.ts"));
    assert!(!prompt.contains("futureOnly"));
}

fn base_workflow_node_with_raw_auto_compact(raw: BTreeMap<String, VmValue>) -> WorkflowNode {
    WorkflowNode {
        id: Some("edit".to_string()),
        kind: "stage".to_string(),
        mode: Some("agent".to_string()),
        done_sentinel: Some("##DONE##".to_string()),
        model_policy: crate::orchestration::ModelPolicy {
            provider: Some("mock".to_string()),
            max_iterations: Some(2),
            ..Default::default()
        },
        auto_compact: crate::orchestration::AutoCompactPolicy {
            enabled: true,
            token_threshold: Some(1),
            compact_strategy: Some("llm".to_string()),
            ..Default::default()
        },
        raw_auto_compact: Some(VmValue::Dict(Rc::new(raw))),
        ..Default::default()
    }
}

fn mock_llm_opts() -> crate::llm::api::LlmCallOptions {
    // The stage builder seeds the provider + model through options so
    // extract_llm_options produces a shape the resolver expects.
    let mut options = BTreeMap::new();
    options.insert(
        "provider".to_string(),
        VmValue::String(Rc::from("mock".to_string())),
    );
    options.insert(
        "model".to_string(),
        VmValue::String(Rc::from("mock-model".to_string())),
    );
    let args = vec![
        VmValue::String(Rc::from(String::new())),
        VmValue::Nil,
        VmValue::Dict(Rc::new(options)),
    ];
    crate::llm::extract_llm_options(&args).expect("mock LlmCallOptions")
}

#[tokio::test(flavor = "current_thread")]
async fn workflow_resolve_stage_auto_compact_forwards_raw_keep_last_and_summary_prompt() {
    crate::reset_thread_local_state();
    let opts = mock_llm_opts();

    let prompt_path = std::env::temp_dir().join(format!(
        "harn-workflow-compaction-summary-{}.harn.prompt",
        uuid::Uuid::now_v7()
    ));
    std::fs::write(&prompt_path, "CUSTOM_WORKFLOW_SUMMARY_PROMPT\n")
        .expect("summary prompt fixture");
    let closure_marker = VmValue::String(Rc::from("compress-callback-sentinel".to_string()));

    let raw = BTreeMap::from([
        ("enabled".to_string(), VmValue::Bool(true)),
        ("token_threshold".to_string(), VmValue::Int(1)),
        (
            "compact_strategy".to_string(),
            VmValue::String(Rc::from("llm".to_string())),
        ),
        ("compact_keep_last".to_string(), VmValue::Int(5)),
        (
            "summarize_prompt".to_string(),
            VmValue::String(Rc::from(prompt_path.to_string_lossy().into_owned())),
        ),
        ("compress_callback".to_string(), closure_marker.clone()),
    ]);
    let node = base_workflow_node_with_raw_auto_compact(raw);

    let config = crate::orchestration::resolve_stage_auto_compact(&node, &opts)
        .await
        .expect("resolves")
        .expect("auto_compact enabled");

    assert_eq!(
        config.keep_last, 5,
        "compact_keep_last from raw dict should override the typed policy default"
    );
    assert_eq!(
        config.summarize_prompt.as_deref(),
        Some(prompt_path.to_string_lossy().as_ref()),
        "summarize_prompt from raw dict should be forwarded verbatim"
    );
    // VmValue is not PartialEq; compare display forms as a sentinel that the
    // same value the host wrote was threaded through to AutoCompactConfig.
    assert_eq!(
        config
            .compress_callback
            .as_ref()
            .map(|v| v.display())
            .as_deref(),
        Some(closure_marker.display().as_str()),
        "compress_callback VmValue should thread through to AutoCompactConfig"
    );

    let _ = std::fs::remove_file(&prompt_path);
}

#[tokio::test(flavor = "current_thread")]
async fn workflow_resolve_stage_auto_compact_accepts_keep_last_alias_and_skips_empty_summary_prompt(
) {
    crate::reset_thread_local_state();
    let opts = mock_llm_opts();

    // `keep_last` is the legacy alias hosts still emit. Empty-string
    // summarize_prompt must leave the default in place (not crash and not
    // become Some("")).
    let raw = BTreeMap::from([
        ("enabled".to_string(), VmValue::Bool(true)),
        ("keep_last".to_string(), VmValue::Int(7)),
        (
            "summarize_prompt".to_string(),
            VmValue::String(Rc::from("   ".to_string())),
        ),
    ]);
    let node = base_workflow_node_with_raw_auto_compact(raw);

    let config = crate::orchestration::resolve_stage_auto_compact(&node, &opts)
        .await
        .expect("resolves")
        .expect("auto_compact enabled");

    assert_eq!(
        config.keep_last, 7,
        "keep_last alias should populate the AutoCompactConfig"
    );
    assert!(
        config.summarize_prompt.is_none(),
        "blank summarize_prompt must stay None rather than becoming an empty override"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn workflow_resolve_stage_auto_compact_returns_none_when_disabled() {
    crate::reset_thread_local_state();
    let opts = mock_llm_opts();

    let mut node = base_workflow_node_with_raw_auto_compact(BTreeMap::new());
    node.auto_compact.enabled = false;

    let config = crate::orchestration::resolve_stage_auto_compact(&node, &opts)
        .await
        .expect("resolves");
    assert!(
        config.is_none(),
        "disabled auto_compact must return None so the agent loop skips compaction wiring"
    );
}
