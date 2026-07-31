//! The scoped event-capture primitive used by Harn orchestration tests and
//! workflows.

use std::sync::Arc;

use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

use super::agent_runtime;

#[derive(Clone)]
struct CapturingAgentEventSink {
    session_id: String,
    events: Arc<std::sync::Mutex<Vec<crate::agent_events::AgentEvent>>>,
}

impl crate::agent_events::AgentEventSink for CapturingAgentEventSink {
    fn handle_event(&self, event: &crate::agent_events::AgentEvent) {
        if event.session_id() != self.session_id {
            return;
        }
        // Fixture records and queue checkpoints are harness audit telemetry,
        // not application checkpoints. Keep them on the external event
        // stream without changing the stable `agent_capture_events` contract
        // that captures events a Harn program emitted for its own workflow.
        let harness_mock_telemetry = match event {
            crate::agent_events::AgentEvent::TypedCheckpoint { checkpoint, .. } => {
                matches!(
                    checkpoint.get("kind").and_then(serde_json::Value::as_str),
                    Some("llm_mock_fixture_consumption") | Some("llm_mock_queue")
                )
            }
            _ => false,
        };
        if harness_mock_telemetry {
            return;
        }
        if let Ok(mut events) = self.events.lock() {
            events.push(event.clone());
        }
    }
}

/// Capture agent events emitted while executing a Harn closure.
#[harn_builtin(
    sig = "__host_agent_capture_events(session_id: string, body: closure) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
pub(super) async fn host_agent_capture_events_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(text)) if !text.is_empty() => text.to_string(),
        Some(VmValue::String(_)) => {
            return Err(VmError::Runtime(
                "__host_agent_capture_events(session_id, body): session_id must be non-empty"
                    .to_string(),
            ))
        }
        Some(other) => {
            let type_name = other.type_name();
            return Err(VmError::Runtime(format!(
                "__host_agent_capture_events(session_id, body): session_id must be a string; got {type_name}"
            )));
        }
        None => {
            return Err(VmError::Runtime(
                "__host_agent_capture_events(session_id, body): missing session_id".to_string(),
            ))
        }
    };
    let body = match args.get(1) {
        Some(VmValue::Closure(closure)) => closure.clone(),
        _ => {
            return Err(VmError::Runtime(
                "__host_agent_capture_events(session_id, body): body must be a closure".to_string(),
            ))
        }
    };

    let captured_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink: Arc<dyn crate::agent_events::AgentEventSink> = Arc::new(CapturingAgentEventSink {
        session_id,
        events: captured_events.clone(),
    });
    let _guard = agent_runtime::LoopSinkGuard::install(Some(sink));
    let mut child_vm = ctx.child_vm();
    let result = child_vm.call_closure_pub(&body, &[]).await;
    let output = child_vm.take_output();
    ctx.forward_output(&output);
    let result = result?;
    let events = captured_events
        .lock()
        .map(|events| {
            events
                .iter()
                .map(|event| serde_json::to_value(event).unwrap_or(serde_json::Value::Null))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut envelope = crate::value::DictMap::new();
    envelope.insert(crate::value::intern_key("result"), result);
    envelope.insert(
        crate::value::intern_key("events"),
        json_to_vm_value(&serde_json::Value::Array(events)),
    );
    Ok(VmValue::dict(envelope))
}
