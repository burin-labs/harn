use serde_json::json;
use std::sync::{Arc, Mutex};

use crate::agent_events::{register_sink, AgentEvent, AgentEventSink};
use crate::value::VmValue;

use super::super::{host_agent_session_record_assistant_builtin, reset_agent_session_host_state};

struct AssistantEventSink(Arc<Mutex<Vec<AgentEvent>>>);

impl AgentEventSink for AssistantEventSink {
    fn handle_event(&self, event: &AgentEvent) {
        self.0
            .lock()
            .expect("assistant event sink poisoned")
            .push(event.clone());
    }
}

#[test]
fn recording_assistant_text_emits_the_live_message_projection() {
    reset_agent_session_host_state();
    let session_id = crate::agent_sessions::open_or_create_for_test(Some(format!(
        "assistant-message-projection-{}",
        uuid::Uuid::new_v4()
    )));
    let captured = Arc::new(Mutex::new(Vec::new()));
    register_sink(&session_id, Arc::new(AssistantEventSink(captured.clone())));
    let llm_result = crate::stdlib::json_to_vm_value(&json!({
        "provider": "fireworks",
        "model": "accounts/fireworks/models/gpt-oss-120b",
        "text": "ready",
        "native_tool_calls": [],
        "tool_calls": [],
    }));

    host_agent_session_record_assistant_builtin(
        &[VmValue::string(&session_id), llm_result],
        &mut String::new(),
    )
    .expect("assistant turn records");

    let events = captured.lock().expect("assistant event sink poisoned");
    assert_eq!(
        events.len(),
        1,
        "the committed reply must be projected once"
    );
    match &events[0] {
        AgentEvent::AgentMessageChunk {
            session_id: event_session_id,
            content,
        } => {
            assert_eq!(event_session_id, &session_id);
            assert_eq!(content, "ready");
        }
        event => panic!("expected assistant message chunk, got {event:?}"),
    }
}

#[test]
fn recording_empty_assistant_text_emits_no_empty_message_chunk() {
    reset_agent_session_host_state();
    let session_id = crate::agent_sessions::open_or_create_for_test(Some(format!(
        "empty-assistant-message-projection-{}",
        uuid::Uuid::new_v4()
    )));
    let captured = Arc::new(Mutex::new(Vec::new()));
    register_sink(&session_id, Arc::new(AssistantEventSink(captured.clone())));
    let llm_result = crate::stdlib::json_to_vm_value(&json!({
        "provider": "fireworks",
        "model": "accounts/fireworks/models/gpt-oss-120b",
        "text": "",
        "native_tool_calls": [],
        "tool_calls": [],
    }));

    host_agent_session_record_assistant_builtin(
        &[VmValue::string(&session_id), llm_result],
        &mut String::new(),
    )
    .expect("empty assistant turn records");

    assert!(
        captured
            .lock()
            .expect("assistant event sink poisoned")
            .is_empty(),
        "absence of visible output must not be reported as an empty chunk"
    );
}
