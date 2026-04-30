//! End-to-end tests for `error_category` on `ToolCallUpdate` events
//! emitted by `run_tool_dispatch`. We assert against the loop result's
//! `transcript.events` rather than the process-global sink registry —
//! `agent_events::reset_all_sinks()` is invoked by `reset_thread_local_state`
//! in unrelated tests, so a sink registered for our session can be wiped
//! mid-run when those tests race ours on the same process. The
//! transcript travels back to us via the loop's return value, which the
//! reset cannot touch.
//!
//! The transcript-side assertions cover every category the issue calls
//! out: schema_validation, permission_denied, rejected_loop, tool_error.
//! For mcp_server_error / host_bridge_error the wire enum is tested via
//! `from_internal` in `agent_events.rs::tests`, since those failure
//! modes have no in-tree producer to drive end-to-end without standing
//! up a fake bridge.

// Each test holds a `std::sync::Mutex` across `.await` points so the
// thread-local stacks (mocks, policies) and global registries the agent
// loop touches don't interleave with sibling tests running on a shared
// pool. The runtime is `current_thread` and the futures don't yield to
// other lock-taking work, so the standard lint about awaiting under a
// sync mutex doesn't apply here.
#![allow(clippy::await_holding_lock)]

use super::*;
use crate::agent_events::ToolCallErrorCategory;
use std::collections::BTreeMap;

/// Serialize the tests in this module. The agent loop touches several
/// thread-local stacks (mocks, execution policies, approval policies,
/// dynamic permissions) that other tests in this crate also push to —
/// running interleaved on the same OS thread means a leftover policy
/// from a sibling test can mask the failure path we're trying to hit.
fn serialize_tests() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Drain the thread-local policy stacks that other tests in this crate
/// might have left behind. Without this, a leftover `auto_deny=*`
/// approval policy or capability ceiling from a sibling test on the
/// same OS thread short-circuits dispatch before we can observe the
/// expected `error_category`.
fn drain_thread_local_state() {
    use crate::orchestration::{pop_approval_policy, pop_execution_policy};
    for _ in 0..16 {
        pop_execution_policy();
        pop_approval_policy();
    }
}

fn read_tool_registry() -> VmValue {
    let mut tool_params = BTreeMap::new();
    tool_params.insert(
        "path".to_string(),
        VmValue::Dict(Rc::new(BTreeMap::from([(
            "type".to_string(),
            VmValue::String(Rc::from("string")),
        )]))),
    );
    let tool = VmValue::Dict(Rc::new(BTreeMap::from([
        ("name".to_string(), VmValue::String(Rc::from("read"))),
        (
            "description".to_string(),
            VmValue::String(Rc::from("Read a file.")),
        ),
        (
            "parameters".to_string(),
            VmValue::Dict(Rc::new(tool_params)),
        ),
        // Declared executor satisfies the agent_loop pre-flight check
        // (harn#743). These tests assert dispatch-error categorization
        // — schema validation fails before the dispatcher cares about
        // the missing bridge.
        (
            "executor".to_string(),
            VmValue::String(Rc::from("host_bridge")),
        ),
        (
            "host_capability".to_string(),
            VmValue::String(Rc::from("workspace.read_text")),
        ),
    ])));
    VmValue::Dict(Rc::new(BTreeMap::from([(
        "tools".to_string(),
        VmValue::List(Rc::new(vec![tool])),
    )])))
}

fn done_mock() -> crate::llm::mock::LlmMock {
    crate::llm::mock::LlmMock {
        text: "<done>##DONE##</done>".to_string(),
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        error: None,
    }
}

fn tool_call_mock(tool_name: &str, args: serde_json::Value) -> crate::llm::mock::LlmMock {
    crate::llm::mock::LlmMock {
        text: String::new(),
        tool_calls: vec![json!({
            "id": format!("call_{tool_name}"),
            "type": "tool_call",
            "name": tool_name,
            "arguments": args,
        })],
        match_pattern: None,
        consume_on_match: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        error: None,
    }
}

fn assert_tool_execution_with_category(
    result: &serde_json::Value,
    expected_tool: &str,
    expected: ToolCallErrorCategory,
) {
    let events = result["transcript"]["events"]
        .as_array()
        .expect("transcript.events must be an array");
    let matched = events.iter().any(|event| {
        event["kind"] == "tool_execution"
            && event["metadata"]["tool_name"] == expected_tool
            && event["metadata"]["error_category"] == expected.as_str()
    });
    assert!(
        matched,
        "expected a tool_execution event for tool '{expected_tool}' \
         tagged with error_category '{}'; got transcript events: {events:#?}",
        expected.as_str(),
    );
}

#[tokio::test(flavor = "current_thread")]
async fn schema_validation_failure_emits_categorized_event() {
    let _guard = serialize_tests();
    drain_thread_local_state();
    reset_llm_mock_state();
    crate::llm::mock::push_llm_mock(tool_call_mock("read", json!({})));
    crate::llm::mock::push_llm_mock(done_mock());

    let mut opts = base_opts(vec![json!({"role": "user", "content": "read"})]);
    opts.tools = Some(read_tool_registry());
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 2;
    config.tool_format = "native".to_string();
    config.session_id = "test-cat-schema-validation".to_string();

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_tool_execution_with_category(&result, "read", ToolCallErrorCategory::SchemaValidation);

    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_tool_emits_permission_denied_category() {
    // No bridge + no Harn-defined handler → dispatch_tool_execution falls
    // through to the "Tool '<name>' is not available" arm, which returns
    // `Err(VmError::CategorizedError { category: ToolRejected })`. The
    // wire enum collapses ToolRejected to PermissionDenied.
    let _guard = serialize_tests();
    drain_thread_local_state();
    reset_llm_mock_state();
    crate::llm::mock::push_llm_mock(tool_call_mock("nonexistent_tool", json!({"arg": "value"})));
    crate::llm::mock::push_llm_mock(done_mock());

    let mut opts = base_opts(vec![json!({"role": "user", "content": "do work"})]);
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 2;
    config.tool_format = "native".to_string();
    config.session_id = "test-cat-permission-denied-unknown".to_string();

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_tool_execution_with_category(
        &result,
        "nonexistent_tool",
        ToolCallErrorCategory::PermissionDenied,
    );

    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_loop_skip_emits_rejected_loop_category() {
    // Three identical tool calls with skip_threshold=2 — the third is
    // skipped by the loop detector. We use `read_file` (handled locally
    // without a bridge) so the calls actually execute and the loop
    // tracker can record an identical-result repeat — repeats of
    // *rejected* tools never reach `loop_tracker.record()`.
    let _guard = serialize_tests();
    drain_thread_local_state();
    reset_llm_mock_state();
    let args = json!({"path": "/nonexistent/test/path/that/never/exists"});
    for _ in 0..3 {
        crate::llm::mock::push_llm_mock(tool_call_mock("read_file", args.clone()));
    }
    crate::llm::mock::push_llm_mock(done_mock());

    let mut opts = base_opts(vec![json!({"role": "user", "content": "read"})]);
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 4;
    config.tool_format = "native".to_string();
    config.session_id = "test-cat-rejected-loop".to_string();
    // Enable detector and arrange for Skip on the third repeat.
    config.loop_detect_warn = 1;
    config.loop_detect_block = 2;
    config.loop_detect_skip = 2;

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_tool_execution_with_category(&result, "read_file", ToolCallErrorCategory::RejectedLoop);

    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
async fn tool_returning_error_string_emits_tool_error_category() {
    // The local `read_file` handler responds with `Some("Error: cannot
    // read file ...")` for an unreadable path. dispatch sees `Ok(...)`
    // — not rejected, not a denied dict — but the result_text starts
    // with "Error:". Final emission is Failed + ToolError.
    let _guard = serialize_tests();
    drain_thread_local_state();
    reset_llm_mock_state();
    crate::llm::mock::push_llm_mock(tool_call_mock(
        "read_file",
        json!({"path": "/nonexistent/path/for/tool_error_test"}),
    ));
    crate::llm::mock::push_llm_mock(done_mock());

    let mut opts = base_opts(vec![json!({"role": "user", "content": "read"})]);
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 2;
    config.tool_format = "native".to_string();
    config.session_id = "test-cat-tool-error".to_string();

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_tool_execution_with_category(&result, "read_file", ToolCallErrorCategory::ToolError);

    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
async fn parse_error_emits_schema_validation_category() {
    // The provider can deliver a malformed tool_calls payload — the
    // VM normalizes that into args carrying a `__parse_error` sentinel,
    // which the dispatch loop short-circuits as a SchemaValidation
    // failure before any policy/permission/validation runs.
    let _guard = serialize_tests();
    drain_thread_local_state();
    reset_llm_mock_state();
    crate::llm::mock::push_llm_mock(tool_call_mock(
        "read",
        json!({"__parse_error": "could not decode arguments JSON"}),
    ));
    crate::llm::mock::push_llm_mock(done_mock());

    let mut opts = base_opts(vec![json!({"role": "user", "content": "read"})]);
    opts.tools = Some(read_tool_registry());
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 2;
    config.tool_format = "native".to_string();
    config.session_id = "test-cat-parse-error".to_string();

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_tool_execution_with_category(&result, "read", ToolCallErrorCategory::SchemaValidation);

    reset_llm_mock_state();
}
