use super::*;
use crate::event_log::EventLog;
use crate::llm::mock::{push_llm_mock, reset_llm_mock_state, LlmMock};

fn assistant_message(text: &str) -> VmValue {
    VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("role"),
            VmValue::String(arcstr::ArcStr::from("assistant")),
        ),
        (
            crate::value::intern_key("content"),
            VmValue::String(arcstr::ArcStr::from(text)),
        ),
    ]))
}

#[tokio::test(flavor = "current_thread")]
async fn execute_sub_agent_persists_one_stop_with_lineage() {
    crate::agent_sessions::reset_session_store();
    reset_llm_mock_state();
    let parent = crate::agent_sessions::open_or_create(Some("parent-subagent".into()));
    crate::agent_events::clear_session_sinks(&parent);
    let lifecycle_log = std::sync::Arc::new(crate::event_log::AnyEventLog::Memory(
        crate::event_log::MemoryEventLog::new(16),
    ));
    crate::agent_events::register_sink(
        parent.clone(),
        crate::agent_events::EventLogSink::new(lifecycle_log.clone(), parent.clone()),
    );
    let parent_chain = crate::ActorChain::new("user:kenneth").pushed("agent:root");
    crate::agent_sessions::set_actor_chain(&parent, Some(parent_chain)).unwrap();
    crate::agent_sessions::inject_message(&parent, assistant_message("parent context")).unwrap();
    crate::agent_sessions::claim_tool_format(&parent, "text").unwrap();
    push_llm_mock(LlmMock {
        text: "child result".to_string(),
        tool_calls: Vec::new(),
        raw_tool_calls: Vec::new(),
        match_pattern: None,
        scope: crate::llm::mock::DEFAULT_MOCK_SCOPE.to_string(),
        entry_id: String::new(),
        sticky: false,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        simulated_cost_usd: None,
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        model: "mock".to_string(),
        provider: None,
        blocks: None,
        logprobs: Vec::new(),
        error: None,
        stream_chunks: Vec::new(),
    });
    let spec = SubAgentRunSpec {
        name: "research-worker".to_string(),
        task: "inspect the repo".to_string(),
        system: None,
        options: crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("mock")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("mock")),
            ),
            (crate::value::intern_key("max_iterations"), VmValue::Int(1)),
        ]),
        returns_schema: None,
        session_id: "child-subagent".to_string(),
        run_id: "agent_run_child_subagent".to_string(),
        parent_session_id: Some(parent.clone()),
        parent_run_id: Some("agent_run_parent_subagent".to_string()),
        reminder_propagation: Vec::new(),
        workspace_anchor: None,
        stop_emitted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
    let result = execute_sub_agent(&ctx, spec).await.unwrap();
    assert_eq!(result.payload["ok"].as_bool(), Some(true));
    let child_run_id = result.payload["run_id"]
        .as_str()
        .expect("sub-agent result must expose its canonical run id")
        .to_string();
    assert_eq!(child_run_id, "agent_run_child_subagent");

    let child_messages = crate::agent_sessions::messages_json("child-subagent");
    assert!(!child_messages
        .iter()
        .any(|message| message["content"].as_str() == Some("parent context")));
    assert_eq!(
        crate::agent_sessions::tool_format("child-subagent").as_deref(),
        Some("json")
    );
    assert_eq!(
        crate::agent_sessions::actor_chain("child-subagent").map(|chain| chain.to_json_value()),
        Some(serde_json::json!({
            "sub": "user:kenneth",
            "act": {
                "sub": "research-worker",
                "act": {"sub": "agent:root"}
            }
        }))
    );

    let parent_events = crate::agent_sessions::snapshot(&parent)
        .and_then(|value| value.as_dict().cloned())
        .and_then(|dict| dict.get("events").cloned())
        .and_then(|value| match value {
            VmValue::List(list) => Some((*list).clone()),
            _ => None,
        })
        .expect("parent events");
    let event_kinds: Vec<String> = parent_events
        .iter()
        .filter_map(|event| event.as_dict())
        .filter_map(|dict| dict.get("kind").map(VmValue::display))
        .collect();
    assert!(event_kinds.iter().any(|kind| kind == "sub_agent_start"));
    assert!(event_kinds.iter().any(|kind| kind == "sub_agent_result"));

    crate::agent_events::flush_session_sinks(&parent)
        .await
        .expect("flush subagent lifecycle");
    let topic =
        crate::event_log::Topic::new(format!("observability.agent_events.{parent}")).unwrap();
    let lifecycle_events = lifecycle_log.read_range(&topic, None, 16).await.unwrap();
    let stops: Vec<_> = lifecycle_events
        .iter()
        .filter(|(_, event)| event.kind == "subagent_stop")
        .collect();
    assert_eq!(stops.len(), 1, "terminal event must persist exactly once");
    let replayed: crate::agent_events::AgentEvent =
        serde_json::from_value(stops[0].1.payload["event"].clone()).unwrap();
    match replayed {
        crate::agent_events::AgentEvent::SubagentStop {
            lineage,
            parent_run_id,
            child_run_id: stopped_child_run_id,
            terminal_status,
            completed_at_ms,
            ..
        } => {
            let lineage = lineage.expect("current stop events carry typed lineage");
            assert_eq!(lineage.parent.session_id, parent);
            assert_eq!(lineage.parent.run_id, "agent_run_parent_subagent");
            assert_eq!(lineage.child.session_id, "child-subagent");
            assert_eq!(lineage.child.run_id, child_run_id);
            assert_eq!(parent_run_id, "agent_run_parent_subagent");
            assert_eq!(stopped_child_run_id, child_run_id);
            assert_ne!(stopped_child_run_id, "child-subagent");
            assert_eq!(
                terminal_status,
                crate::agent_events::SubagentTerminalStatus::Success
            );
            assert!(completed_at_ms > 0);
        }
        other => panic!("expected replayed SubagentStop, got {other:?}"),
    }
    crate::agent_events::clear_session_sinks(&parent);
    reset_llm_mock_state();
    crate::agent_sessions::reset_session_store();
}
