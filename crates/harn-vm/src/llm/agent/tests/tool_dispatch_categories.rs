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
    tool_call_with_text_mock(String::new(), tool_name, args)
}

fn tool_call_with_text_mock(
    text: String,
    tool_name: &str,
    args: serde_json::Value,
) -> crate::llm::mock::LlmMock {
    crate::llm::mock::LlmMock {
        text,
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

fn text_mock(text: &str) -> crate::llm::mock::LlmMock {
    crate::llm::mock::LlmMock {
        text: text.to_string(),
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

fn native_read_file_schema() -> serde_json::Value {
    json!({
        "type": "function",
        "function": {
            "name": "read_file",
            "description": "Read a file.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }
        }
    })
}

fn text_read_file_registry() -> VmValue {
    let mut path_param = BTreeMap::new();
    path_param.insert("type".to_string(), VmValue::String(Rc::from("string")));
    let mut params = BTreeMap::new();
    params.insert("path".to_string(), VmValue::Dict(Rc::new(path_param)));
    let tool = VmValue::Dict(Rc::new(BTreeMap::from([
        ("name".to_string(), VmValue::String(Rc::from("read_file"))),
        (
            "description".to_string(),
            VmValue::String(Rc::from("Read a file.")),
        ),
        ("parameters".to_string(), VmValue::Dict(Rc::new(params))),
        ("executor".to_string(), VmValue::String(Rc::from("harn"))),
    ])));
    VmValue::Dict(Rc::new(BTreeMap::from([(
        "tools".to_string(),
        VmValue::List(Rc::new(vec![tool])),
    )])))
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
    let guard = serialize_tests();
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
    drop(guard);
}

#[tokio::test(flavor = "current_thread")]
async fn completion_judge_can_confirm_successful_tool_turn_from_transcript_context() {
    let guard = serialize_tests();
    drain_thread_local_state();
    reset_llm_mock_state();
    let temp_file = tempfile::NamedTempFile::new().expect("create temp file");
    let path = temp_file.path().to_path_buf();
    std::fs::write(&path, "COMPLETE-OK\n").expect("write temp file");

    crate::llm::mock::push_llm_mock(tool_call_with_text_mock(
        "The validation file contains COMPLETE-OK, so the request is satisfied.".to_string(),
        "read_file",
        json!({"path": path.to_string_lossy().to_string()}),
    ));
    crate::llm::mock::push_llm_mock(text_mock(
        "{\"pass\": true, \"final_response\": \"Confirmed: the validation file contains COMPLETE-OK.\"}",
    ));
    crate::llm::mock::push_llm_mock(text_mock("unexpected extra turn"));

    let mut opts = base_opts(vec![json!({
        "role": "user",
        "content": "Read the validation file and stop once COMPLETE-OK is observed.",
    })]);
    opts.native_tools = Some(vec![native_read_file_schema()]);
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 3;
    config.max_verify_attempts = 2;
    config.tool_format = "native".to_string();
    config.verify_completion_judge =
        Some(crate::llm::agent::completion_judge::CompletionJudgeConfig::default());
    config.session_id = "test-judge-successful-tool-turn".to_string();

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    let calls = get_llm_mock_calls();
    assert_eq!(
        calls.len(),
        2,
        "successful tool turn should be verified instead of forcing another main-model turn"
    );
    let judge_prompt = calls[1]
        .messages
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        judge_prompt.contains("Read the validation file"),
        "judge prompt should include the original user request: {judge_prompt}"
    );
    assert!(
        judge_prompt.contains("COMPLETE-OK"),
        "judge prompt should include recent tool-result context: {judge_prompt}"
    );
    assert_eq!(result["status"], "done");
    let visible_text = result["visible_text"]
        .as_str()
        .expect("verified tool turn should produce visible text");
    assert_eq!(
        visible_text,
        "Confirmed: the validation file contains COMPLETE-OK."
    );

    std::fs::remove_file(path).ok();
    reset_llm_mock_state();
    drop(guard);
}

#[tokio::test(flavor = "current_thread")]
async fn completion_judge_accepts_long_final_response() {
    let guard = serialize_tests();
    drain_thread_local_state();
    reset_llm_mock_state();
    let temp_file = tempfile::NamedTempFile::new().expect("create temp file");
    let path = temp_file.path().to_path_buf();
    std::fs::write(&path, "LONG-ANSWER\n").expect("write temp file");
    let final_response = format!(
        "Confirmed LONG-ANSWER. {}",
        "This sentence keeps a useful but lengthy final answer valid. ".repeat(70)
    );
    let judge_json = serde_json::json!({
        "pass": true,
        "final_response": final_response,
    })
    .to_string();

    crate::llm::mock::push_llm_mock(tool_call_with_text_mock(
        "I will inspect the file.".to_string(),
        "read_file",
        json!({"path": path.to_string_lossy().to_string()}),
    ));
    crate::llm::mock::push_llm_mock(text_mock(&judge_json));
    crate::llm::mock::push_llm_mock(text_mock("unexpected extra turn"));

    let mut opts = base_opts(vec![json!({
        "role": "user",
        "content": "Read the validation file and summarize it fully.",
    })]);
    opts.native_tools = Some(vec![native_read_file_schema()]);
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 3;
    config.max_verify_attempts = 2;
    config.tool_format = "native".to_string();
    config.verify_completion_judge =
        Some(crate::llm::agent::completion_judge::CompletionJudgeConfig::default());
    config.session_id = "test-judge-long-final-response".to_string();

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    let calls = get_llm_mock_calls();
    assert_eq!(
        calls.len(),
        2,
        "long but valid judge final_response should not be schema-vetoed"
    );
    assert_eq!(result["status"], "done");
    assert_eq!(
        result["visible_text"].as_str(),
        Some(final_response.trim_end())
    );

    std::fs::remove_file(path).ok();
    reset_llm_mock_state();
    drop(guard);
}

#[tokio::test(flavor = "current_thread")]
async fn completion_judge_treats_null_optional_fields_as_absent() {
    let guard = serialize_tests();
    drain_thread_local_state();
    reset_llm_mock_state();
    let temp_file = tempfile::NamedTempFile::new().expect("create temp file");
    let path = temp_file.path().to_path_buf();
    std::fs::write(&path, "NEEDS-ANSWER\n").expect("write temp file");

    crate::llm::mock::push_llm_mock(tool_call_with_text_mock(
        "I will inspect the file before answering.".to_string(),
        "read_file",
        json!({"path": path.to_string_lossy().to_string()}),
    ));
    crate::llm::mock::push_llm_mock(text_mock(
        "{\"pass\": false, \"feedback\": null, \"final_response\": null}",
    ));
    crate::llm::mock::push_llm_mock(text_mock("The file contains NEEDS-ANSWER."));
    crate::llm::mock::push_llm_mock(text_mock(
        "{\"pass\": true, \"final_response\": \"Confirmed after judge feedback: NEEDS-ANSWER.\"}",
    ));
    crate::llm::mock::push_llm_mock(text_mock("unexpected extra turn"));

    let mut opts = base_opts(vec![json!({
        "role": "user",
        "content": "Read the validation file and answer with its token.",
    })]);
    opts.native_tools = Some(vec![native_read_file_schema()]);
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 4;
    config.max_verify_attempts = 3;
    config.tool_format = "native".to_string();
    config.verify_completion_judge =
        Some(crate::llm::agent::completion_judge::CompletionJudgeConfig::default());
    config.session_id = "test-judge-null-optional-fields".to_string();

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    let calls = get_llm_mock_calls();
    assert_eq!(
        calls.len(),
        4,
        "null optional judge fields should not trigger a schema retry"
    );
    let resumed_prompt = calls[2]
        .messages
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        resumed_prompt.contains("The completion judge was not satisfied"),
        "null feedback should fall back to the configured feedback instruction: {resumed_prompt}"
    );
    assert_eq!(result["status"], "done");
    let visible_text = result["visible_text"]
        .as_str()
        .expect("verified retry should produce visible text");
    assert_eq!(
        visible_text,
        "Confirmed after judge feedback: NEEDS-ANSWER."
    );

    std::fs::remove_file(path).ok();
    reset_llm_mock_state();
    drop(guard);
}

#[tokio::test(flavor = "current_thread")]
async fn completion_judge_schema_failure_vetoes_with_fallback() {
    let guard = serialize_tests();
    drain_thread_local_state();
    reset_llm_mock_state();
    let temp_file = tempfile::NamedTempFile::new().expect("create temp file");
    let path = temp_file.path().to_path_buf();
    std::fs::write(&path, "SCHEMA-RECOVERY\n").expect("write temp file");

    crate::llm::mock::push_llm_mock(tool_call_with_text_mock(
        "I will inspect the file before answering.".to_string(),
        "read_file",
        json!({"path": path.to_string_lossy().to_string()}),
    ));
    crate::llm::mock::push_llm_mock(text_mock("not json"));
    crate::llm::mock::push_llm_mock(text_mock("The file contains SCHEMA-RECOVERY."));
    crate::llm::mock::push_llm_mock(text_mock(
        "{\"pass\": true, \"final_response\": \"Recovered after fallback feedback: SCHEMA-RECOVERY.\"}",
    ));
    crate::llm::mock::push_llm_mock(text_mock("unexpected extra turn"));

    let mut opts = base_opts(vec![json!({
        "role": "user",
        "content": "Read the validation file and answer with its token.",
    })]);
    opts.native_tools = Some(vec![native_read_file_schema()]);
    let mut judge_config = crate::llm::agent::completion_judge::CompletionJudgeConfig::default();
    judge_config
        .options
        .insert("schema_retries".to_string(), VmValue::Int(0));
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 4;
    config.max_verify_attempts = 3;
    config.tool_format = "native".to_string();
    config.verify_completion_judge = Some(judge_config);
    config.session_id = "test-judge-schema-failure-fallback".to_string();

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    let calls = get_llm_mock_calls();
    assert_eq!(
        calls.len(),
        4,
        "invalid judge JSON should veto once, not confirm completion or retry schema internally"
    );
    let resumed_prompt = calls[2]
        .messages
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        resumed_prompt.contains("The completion judge was not satisfied"),
        "schema failure should inject fallback feedback into the next main-model turn: {resumed_prompt}"
    );
    assert_eq!(result["status"], "done");
    let visible_text = result["visible_text"]
        .as_str()
        .expect("verified retry should produce visible text");
    assert_eq!(
        visible_text,
        "Recovered after fallback feedback: SCHEMA-RECOVERY."
    );

    std::fs::remove_file(path).ok();
    reset_llm_mock_state();
    drop(guard);
}

#[tokio::test(flavor = "current_thread")]
async fn native_persistent_answer_after_successful_tool_does_not_require_done_sentinel() {
    let guard = serialize_tests();
    drain_thread_local_state();
    reset_llm_mock_state();
    let temp_file = tempfile::NamedTempFile::new().expect("create temp file");
    let path = temp_file.path().to_path_buf();
    std::fs::write(&path, "@burin/tui\n").expect("write temp file");

    crate::llm::mock::push_llm_mock(tool_call_mock(
        "read_file",
        json!({"path": path.to_string_lossy().to_string()}),
    ));
    crate::llm::mock::push_llm_mock(text_mock("@burin/tui"));
    crate::llm::mock::push_llm_mock(text_mock("unexpected extra turn"));

    let mut opts = base_opts(vec![
        json!({"role": "user", "content": "read package name"}),
    ]);
    opts.native_tools = Some(vec![native_read_file_schema()]);
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 3;
    config.tool_format = "native".to_string();
    config.turn_policy = Some(TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: true,
        max_prose_chars: None,
    });
    config.session_id = "test-native-final-after-tool".to_string();

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    let calls = get_llm_mock_calls();
    assert_eq!(calls.len(), 2, "native final answer should stop the loop");
    assert_eq!(result["text"].as_str(), Some("@burin/tui"));

    let _ = std::fs::remove_file(path);
    reset_llm_mock_state();
    drop(guard);
}

#[tokio::test(flavor = "current_thread")]
async fn text_persistent_bare_answer_after_successful_tool_is_final_response() {
    let guard = serialize_tests();
    drain_thread_local_state();
    reset_llm_mock_state();
    let temp_file = tempfile::NamedTempFile::new().expect("create temp file");
    let path = temp_file.path().to_path_buf();
    std::fs::write(&path, "@burin/tui\n").expect("write temp file");
    let escaped_path = path
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");

    crate::llm::mock::push_llm_mock(text_mock(&format!(
        "<tool_call>\nread_file({{ path: \"{escaped_path}\" }})\n</tool_call>"
    )));
    crate::llm::mock::push_llm_mock(text_mock("@burin/tui"));
    crate::llm::mock::push_llm_mock(text_mock("unexpected extra turn"));

    let mut opts = base_opts(vec![
        json!({"role": "user", "content": "read package name"}),
    ]);
    opts.tools = Some(text_read_file_registry());
    let mut config = base_agent_config();
    config.persistent = true;
    config.max_iterations = 3;
    config.tool_format = "text".to_string();
    config.turn_policy = Some(TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: true,
        max_prose_chars: None,
    });
    config.session_id = "test-text-final-after-tool".to_string();

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    let calls = get_llm_mock_calls();
    assert_eq!(calls.len(), 2, "text final answer should stop the loop");
    assert_eq!(result["visible_text"].as_str(), Some("@burin/tui"));

    let _ = std::fs::remove_file(path);
    reset_llm_mock_state();
    drop(guard);
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_tool_emits_permission_denied_category() {
    // No bridge + no Harn-defined handler → dispatch_tool_execution falls
    // through to the "Tool '<name>' is not available" arm, which returns
    // `Err(VmError::CategorizedError { category: ToolRejected })`. The
    // wire enum collapses ToolRejected to PermissionDenied.
    let guard = serialize_tests();
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
    drop(guard);
}

#[tokio::test(flavor = "current_thread")]
async fn rejected_loop_skip_emits_rejected_loop_category() {
    // Three identical tool calls with skip_threshold=2 — the third is
    // skipped by the loop detector. We use `read_file` (handled locally
    // without a bridge) so the calls actually execute and the loop
    // tracker can record an identical-result repeat — repeats of
    // *rejected* tools never reach `loop_tracker.record()`.
    let guard = serialize_tests();
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
    drop(guard);
}

#[tokio::test(flavor = "current_thread")]
async fn tool_returning_error_string_emits_tool_error_category() {
    // The local `read_file` handler responds with `Some("Error: cannot
    // read file ...")` for an unreadable path. dispatch sees `Ok(...)`
    // — not rejected, not a denied dict — but the result_text starts
    // with "Error:". Final emission is Failed + ToolError.
    let guard = serialize_tests();
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
    drop(guard);
}

#[tokio::test(flavor = "current_thread")]
async fn parse_error_emits_schema_validation_category() {
    // The provider can deliver a malformed tool_calls payload — the
    // VM normalizes that into args carrying a `__parse_error` sentinel,
    // which the dispatch loop short-circuits as a SchemaValidation
    // failure before any policy/permission/validation runs.
    let guard = serialize_tests();
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
    drop(guard);
}
