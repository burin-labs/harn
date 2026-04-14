//! ACP AgentEventSink — translates canonical `AgentEvent` variants into ACP
//! `session/update` notifications. Registered per-session at prompt start.

use std::io::Write;
use std::sync::Arc;

use harn_vm::agent_events::{AgentEvent, AgentEventSink};
use harn_vm::visible_text::sanitize_visible_assistant_text;

/// Writes canonical ACP `session/update` notifications for each
/// `AgentEvent` the turn loop emits. Holds only the minimum state needed
/// to serialize notifications without the full AcpBridge.
pub(super) struct AcpAgentEventSink {
    stdout_lock: Arc<std::sync::Mutex<()>>,
}

impl AcpAgentEventSink {
    pub(super) fn new(stdout_lock: Arc<std::sync::Mutex<()>>) -> Self {
        Self { stdout_lock }
    }

    fn write_notification(&self, params: serde_json::Value) {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": params,
        });
        if let Ok(line) = serde_json::to_string(&notification) {
            let _guard = self.stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut stdout = std::io::stdout().lock();
            let _ = stdout.write_all(line.as_bytes());
            let _ = stdout.write_all(b"\n");
            let _ = stdout.flush();
        }
    }

    fn status_str(status: harn_vm::agent_events::ToolCallStatus) -> &'static str {
        use harn_vm::agent_events::ToolCallStatus::*;
        match status {
            Pending => "pending",
            InProgress => "in_progress",
            Completed => "completed",
            Failed => "failed",
        }
    }
}

impl AgentEventSink for AcpAgentEventSink {
    fn handle_event(&self, event: &AgentEvent) {
        match event {
            AgentEvent::AgentMessageChunk {
                session_id,
                content,
            } => {
                let visible = sanitize_visible_assistant_text(content, true);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {
                            "type": "text",
                            "text": content,
                            "visible_text": visible.clone(),
                            "visible_delta": visible,
                        },
                    },
                }));
            }
            AgentEvent::AgentThoughtChunk {
                session_id,
                content,
            } => {
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_thought_chunk",
                        "content": {
                            "type": "text",
                            "text": content,
                        },
                    },
                }));
            }
            AgentEvent::ToolCall {
                session_id,
                tool_call_id,
                tool_name,
                kind,
                status,
                raw_input,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": tool_call_id,
                    "title": tool_name,
                    "status": Self::status_str(*status),
                    "rawInput": raw_input,
                });
                if let Some(k) = kind {
                    update["kind"] = serde_json::to_value(k).unwrap_or_default();
                }
                self.write_notification(serde_json::json!({
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
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": tool_call_id,
                    "title": tool_name,
                    "status": Self::status_str(*status),
                });
                if let Some(out) = raw_output {
                    update["rawOutput"] = out.clone();
                }
                if let Some(err) = error {
                    update["error"] = serde_json::Value::String(err.clone());
                }
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::Plan { session_id, plan } => {
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "plan",
                        "plan": plan,
                    },
                }));
            }
            // Pipeline-loop milestones with no canonical ACP session/update
            // mapping; deliberately not forwarded.
            AgentEvent::TurnStart { .. }
            | AgentEvent::TurnEnd { .. }
            | AgentEvent::FeedbackInjected { .. }
            | AgentEvent::BudgetExhausted { .. }
            | AgentEvent::LoopStuck { .. }
            | AgentEvent::DaemonWatchdogTripped { .. } => {}
        }
    }
}
