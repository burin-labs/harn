//! Tests for the timing fields added to `AgentEvent::ToolCallUpdate`
//! (#689). Two invariants under verification:
//!
//! 1. The terminal `Completed`/`Failed` update carries both
//!    `duration_ms` (parse-to-finish total) and `execution_duration_ms`
//!    (inner host/builtin/MCP dispatch only).
//! 2. Intermediate `Pending`/`InProgress` updates leave both as `None`
//!    so ACP clients can render "still running" without inventing a
//!    bogus zero duration.
//!
//! The test drives the real `run_agent_loop_internal` with a mock LLM
//! that emits one `read_file` text-tool call against a temp file. That
//! exercises the local-tool dispatch path end-to-end without touching
//! the network or the host bridge.
//!
//! Events are captured via the per-loop `AgentLoopConfig.event_sink`
//! (a thread-local installed by the loop entry) rather than the global
//! `agent_events` registry, so concurrent tests that call
//! `reset_all_sinks` / `reset_thread_local_state` cannot race the
//! observation away mid-loop.

use super::*;
use crate::agent_events::{AgentEvent, AgentEventSink, ToolCallStatus};
use std::sync::Mutex as StdMutex;

struct CapturingSink {
    events: Arc<StdMutex<Vec<AgentEvent>>>,
}

impl AgentEventSink for CapturingSink {
    fn handle_event(&self, event: &AgentEvent) {
        self.events
            .lock()
            .expect("event capture mutex poisoned")
            .push(event.clone());
    }
}

fn read_file_tool_decl() -> VmValue {
    let mut path_param = std::collections::BTreeMap::new();
    path_param.insert("type".to_string(), VmValue::String(Rc::from("string")));
    let mut params = std::collections::BTreeMap::new();
    params.insert("path".to_string(), VmValue::Dict(Rc::new(path_param)));
    let mut tool = std::collections::BTreeMap::new();
    tool.insert("name".to_string(), VmValue::String(Rc::from("read_file")));
    tool.insert(
        "description".to_string(),
        VmValue::String(Rc::from("Read a file.")),
    );
    tool.insert("parameters".to_string(), VmValue::Dict(Rc::new(params)));
    // `read_file` is a VM-stdlib short-circuit served by
    // `handle_tool_locally`, so the executor declaration satisfies
    // the agent_loop pre-flight check (harn#743) without needing a
    // registered handler closure.
    tool.insert("executor".to_string(), VmValue::String(Rc::from("harn")));
    let mut envelope = std::collections::BTreeMap::new();
    envelope.insert(
        "tools".to_string(),
        VmValue::List(Rc::new(vec![VmValue::Dict(Rc::new(tool))])),
    );
    VmValue::Dict(Rc::new(envelope))
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_durations_populated_on_terminal_absent_on_in_progress() {
    reset_llm_mock_state();

    // Stage a temp file the local `read_file` handler can read.
    let dir =
        std::env::temp_dir().join(format!("harn-tool-duration-test-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let file_path = dir.join("hello.txt");
    std::fs::write(&file_path, "hello world\n").expect("write temp file");
    let file_path_str = file_path.to_string_lossy().into_owned();

    // Mock LLM emits a single text-format `read_file` tool call. The
    // agent loop dispatches it locally and then exhausts its budget.
    let response_text = format!(
        "<tool_call>\nread_file({{ path: \"{}\" }})\n</tool_call>",
        file_path_str.replace('\\', "\\\\").replace('"', "\\\"")
    );
    crate::llm::mock::push_llm_mock(crate::llm::mock::LlmMock {
        text: response_text,
        tool_calls: Vec::new(),
        match_pattern: None,
        consume_on_match: true,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        thinking: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        error: None,
    });

    let captured: Arc<StdMutex<Vec<AgentEvent>>> = Arc::new(StdMutex::new(Vec::new()));
    let sink = Arc::new(CapturingSink {
        events: captured.clone(),
    });

    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "read the staged file",
    })]);
    opts.tools = Some(read_file_tool_decl());

    let mut config = base_agent_config();
    config.session_id = format!("tool-duration-test-{}", uuid::Uuid::now_v7());
    config.persistent = true;
    config.max_iterations = 1;
    config.event_sink = Some(sink);

    let _ = run_agent_loop_internal(&mut opts, config)
        .await
        .expect("agent loop runs");

    let events = captured.lock().expect("captured events mutex poisoned");
    let updates: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallUpdate {
                tool_name,
                status,
                duration_ms,
                execution_duration_ms,
                ..
            } if tool_name == "read_file" => Some((*status, *duration_ms, *execution_duration_ms)),
            _ => None,
        })
        .collect();

    assert!(
        !updates.is_empty(),
        "expected at least one read_file ToolCallUpdate, got events: {:#?}",
        events
    );

    let in_progress_count = updates
        .iter()
        .filter(|(s, _, _)| matches!(s, ToolCallStatus::InProgress))
        .count();
    let completed_updates: Vec<_> = updates
        .iter()
        .filter(|(s, _, _)| matches!(s, ToolCallStatus::Completed))
        .collect();
    assert!(
        in_progress_count >= 1,
        "expected an InProgress update before Completed; got: {updates:?}"
    );
    assert_eq!(
        completed_updates.len(),
        1,
        "expected exactly one Completed terminal update; got: {updates:?}"
    );

    for (status, duration_ms, exec_ms) in &updates {
        match status {
            ToolCallStatus::Pending | ToolCallStatus::InProgress => {
                assert!(
                    duration_ms.is_none(),
                    "intermediate {status:?} update must not carry duration_ms; got {duration_ms:?}"
                );
                assert!(
                    exec_ms.is_none(),
                    "intermediate {status:?} update must not carry execution_duration_ms; got {exec_ms:?}"
                );
            }
            ToolCallStatus::Completed | ToolCallStatus::Failed => {
                assert!(
                    duration_ms.is_some(),
                    "terminal {status:?} update must carry duration_ms"
                );
                assert!(
                    exec_ms.is_some(),
                    "terminal {status:?} update must carry execution_duration_ms"
                );
                let total = duration_ms.unwrap();
                let exec = exec_ms.unwrap();
                // duration_ms is the parse-to-finish window and so must
                // be at least as long as the inner dispatch window. The
                // measurements come from two different `Instant::now()`
                // calls a few statements apart; allow 1ms slop for the
                // edge case where both round to the same millisecond.
                assert!(
                    total + 1 >= exec,
                    "duration_ms ({total}) should be >= execution_duration_ms ({exec})"
                );
            }
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
    reset_llm_mock_state();
}
