//! Standard tool-call lifecycle updates from canonical agent events.

use super::AcpAgentEventSink;
use harn_vm::agent_events::AgentEvent;

pub(super) fn handle(sink: &AcpAgentEventSink, event: &AgentEvent) {
    match event {
        AgentEvent::ToolCall {
            session_id,
            tool_call_id,
            tool_name,
            kind,
            status,
            raw_input,
            parsing,
            audit,
            intent,
        } => {
            let mut update = serde_json::json!({
                "sessionUpdate": "tool_call",
                "toolCallId": tool_call_id,
                "name": tool_name,
                "title": tool_name,
                "status": AcpAgentEventSink::status_str(*status),
                "rawInput": raw_input,
            });
            if let Some(k) = kind {
                update["kind"] = serde_json::to_value(k).unwrap_or_default();
            }
            let mut harn_meta = serde_json::Map::new();
            if let Some(p) = parsing {
                harn_meta.insert("parsing".to_string(), serde_json::Value::Bool(*p));
            }
            if let Some(record) = audit {
                if let Ok(value) = serde_json::to_value(record) {
                    harn_meta.insert("audit".to_string(), value);
                }
            }
            if let Some(intent) = intent {
                harn_meta.insert("intent".to_string(), serde_json::json!(intent));
            }
            AcpAgentEventSink::attach_harn_meta(&mut update, harn_meta);
            sink.write_notification(serde_json::json!({
                "sessionId": session_id,
                "update": update,
            }));
        }
        AgentEvent::ToolCallUpdate {
            session_id,
            tool_call_id,
            tool_name,
            status,
            raw_output,
            error,
            duration_ms,
            execution_duration_ms,
            error_category,
            mutation_status,
            changed_paths,
            data,
            health,
            executor,
            parsing,
            raw_input,
            raw_input_partial,
            audit,
        } => {
            let mut update = serde_json::json!({
                "sessionUpdate": "tool_call_update",
                "toolCallId": tool_call_id,
                "name": tool_name,
                "title": tool_name,
                "status": AcpAgentEventSink::status_str(*status),
            });
            let mut harn_meta = serde_json::Map::new();
            if let Some(health) = health {
                harn_meta.insert("health".to_string(), serde_json::json!(health));
            }
            if let Some(out) = raw_output {
                update["rawOutput"] = out.clone();
            }
            if let Some(err) = error {
                harn_meta.insert("error".to_string(), serde_json::Value::String(err.clone()));
            }
            if let Some(d) = duration_ms {
                harn_meta.insert("durationMs".to_string(), serde_json::Value::from(*d));
            }
            if let Some(d) = execution_duration_ms {
                harn_meta.insert(
                    "executionDurationMs".to_string(),
                    serde_json::Value::from(*d),
                );
            }
            if let Some(cat) = error_category {
                harn_meta.insert(
                    "errorCategory".to_string(),
                    serde_json::Value::String(cat.as_str().to_string()),
                );
            }
            harn_meta.insert(
                "mutationStatus".to_string(),
                serde_json::Value::String(mutation_status.as_str().to_string()),
            );
            if let Some(paths) = changed_paths {
                harn_meta.insert("changedPaths".to_string(), serde_json::json!(paths));
            }
            if let Some(data) = data {
                harn_meta.insert("data".to_string(), data.clone());
            }
            if let Some(exec) = executor {
                harn_meta.insert(
                    "executor".to_string(),
                    AcpAgentEventSink::executor_to_json(exec),
                );
            }
            if let Some(p) = parsing {
                harn_meta.insert("parsing".to_string(), serde_json::Value::Bool(*p));
            }
            if let Some(record) = audit {
                if let Ok(value) = serde_json::to_value(record) {
                    harn_meta.insert("audit".to_string(), value);
                }
            }
            if let Some(input) = raw_input {
                update["rawInput"] = input.clone();
            }
            if let Some(partial) = raw_input_partial {
                harn_meta.insert(
                    "rawInputPartial".to_string(),
                    serde_json::Value::String(partial.clone()),
                );
            }
            AcpAgentEventSink::attach_harn_meta(&mut update, harn_meta);
            sink.write_notification(serde_json::json!({
                "sessionId": session_id,
                "update": update,
            }));
        }
        _ => {}
    }
}
