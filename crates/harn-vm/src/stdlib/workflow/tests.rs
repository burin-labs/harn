use super::artifact::{load_run_tree, snapshot_trace_spans};
use super::map::{execute_join_tasks, LocalTask};
use super::stage::{execute_stage_attempts, replay_stage};
use crate::orchestration::{
    save_run_record, stage_verification_contracts, verification_contract_from_verify,
    workflow_verification_contracts, RunChildRecord, RunExecutionRecord, RunRecord, RunStageRecord,
    VerificationContract, WorkflowGraph, WorkflowNode,
};
use crate::tracing::{set_tracing_enabled, span_end, span_start, SpanKind};
use crate::value::VmValue;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

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

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn execute_join_tasks_stops_after_first_completion() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let tasks: Vec<LocalTask<i32>> = vec![
                Box::pin(async {
                    // Never resolves on its own; the target count must
                    // abort this branch once the immediate winner completes.
                    std::future::pending::<()>().await;
                    1
                }),
                Box::pin(async { 2 }),
            ];
            let results = execute_join_tasks(tasks, 1, None).await;
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].as_ref().ok().copied(), Some(2));
        })
        .await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn execute_join_tasks_honors_target_and_concurrency_limit() {
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
                        // Yield twice so the executor schedules the
                        // sibling task and `max_seen` observes both
                        // branches in flight before either decrements
                        // `active`.
                        tokio::task::yield_now().await;
                        tokio::task::yield_now().await;
                        active.set(active.get().saturating_sub(1));
                        value
                    }) as LocalTask<i32>
                })
                .collect::<Vec<_>>();
            let results = execute_join_tasks(tasks, 2, Some(2)).await;
            assert_eq!(results.len(), 2);
            assert!(
                max_seen.get() <= 2,
                "observed concurrency {}",
                max_seen.get()
            );
        })
        .await;
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

    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let _vm_context = crate::vm::install_async_builtin_child_vm(vm);
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
        metadata: BTreeMap::from([
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

#[tokio::test(flavor = "current_thread", start_paused = true)]
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

#[tokio::test(flavor = "current_thread", start_paused = true)]
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

#[tokio::test(flavor = "current_thread", start_paused = true)]
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
