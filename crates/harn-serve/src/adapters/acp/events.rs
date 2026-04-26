//! ACP AgentEventSink — translates canonical `AgentEvent` variants into ACP
//! `session/update` notifications. Registered per-session at prompt start.

use harn_vm::agent_events::{AgentEvent, AgentEventSink};
use harn_vm::visible_text::sanitize_visible_assistant_text;

use super::AcpOutput;

/// Writes canonical ACP `session/update` notifications for each
/// `AgentEvent` the turn loop emits. Holds only the minimum state needed
/// to serialize notifications without the full AcpBridge.
pub(super) struct AcpAgentEventSink {
    output: AcpOutput,
}

impl AcpAgentEventSink {
    pub(super) fn new(output: AcpOutput) -> Self {
        Self { output }
    }

    fn write_notification(&self, params: serde_json::Value) {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": params,
        });
        if let Ok(line) = serde_json::to_string(&notification) {
            self.output.write_line(&line);
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
                duration_ms,
                execution_duration_ms,
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
                if let Some(d) = duration_ms {
                    update["durationMs"] = serde_json::Value::from(*d);
                }
                if let Some(d) = execution_duration_ms {
                    update["executionDurationMs"] = serde_json::Value::from(*d);
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
            AgentEvent::SkillActivated {
                session_id,
                skill_name,
                iteration,
                reason,
            } => {
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "skill_activated",
                        "skillName": skill_name,
                        "iteration": iteration,
                        "reason": reason,
                    },
                }));
            }
            AgentEvent::SkillDeactivated {
                session_id,
                skill_name,
                iteration,
            } => {
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "skill_deactivated",
                        "skillName": skill_name,
                        "iteration": iteration,
                    },
                }));
            }
            AgentEvent::SkillScopeTools {
                session_id,
                skill_name,
                allowed_tools,
            } => {
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "skill_scope_tools",
                        "skillName": skill_name,
                        "allowedTools": allowed_tools,
                    },
                }));
            }
            AgentEvent::ToolSearchQuery {
                session_id,
                tool_use_id,
                name,
                query,
                strategy,
                mode,
            } => {
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "tool_search_query",
                        "toolUseId": tool_use_id,
                        "name": name,
                        "query": query,
                        "strategy": strategy,
                        "mode": mode,
                    },
                }));
            }
            AgentEvent::ToolSearchResult {
                session_id,
                tool_use_id,
                promoted,
                strategy,
                mode,
            } => {
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "tool_search_result",
                        "toolUseId": tool_use_id,
                        "promoted": promoted,
                        "strategy": strategy,
                        "mode": mode,
                    },
                }));
            }
            AgentEvent::TranscriptCompacted {
                session_id,
                mode,
                strategy,
                archived_messages,
                estimated_tokens_before,
                estimated_tokens_after,
                snapshot_asset_id,
            } => {
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "transcript_compacted",
                        "mode": mode,
                        "strategy": strategy,
                        "archivedMessages": archived_messages,
                        "estimatedTokensBefore": estimated_tokens_before,
                        "estimatedTokensAfter": estimated_tokens_after,
                        "snapshotAssetId": snapshot_asset_id,
                    },
                }));
            }
            AgentEvent::Handoff {
                session_id,
                artifact_id,
                handoff,
            } => {
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "handoff",
                        "handoffId": handoff.id,
                        "artifactId": artifact_id,
                        "handoff": handoff,
                    },
                }));
            }
            AgentEvent::FsWatch {
                session_id,
                subscription_id,
                events,
            } => {
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "fs_watch",
                        "subscriptionId": subscription_id,
                        "events": events,
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

#[cfg(test)]
mod tests {
    use harn_vm::agent_events::{AgentEvent, AgentEventSink, FsWatchEvent, ToolCallStatus};
    use harn_vm::orchestration::{HandoffArtifact, HandoffTargetRecord};
    use harn_vm::tool_annotations::ToolKind;
    use tokio::sync::mpsc;

    use super::{AcpAgentEventSink, AcpOutput};

    #[tokio::test(flavor = "current_thread")]
    async fn handoff_event_serializes_as_session_update() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        sink.handle_event(&harn_vm::agent_events::AgentEvent::Handoff {
            session_id: "session-1".to_string(),
            artifact_id: "artifact-1".to_string(),
            handoff: Box::new(
                HandoffArtifact {
                    id: "handoff-1".to_string(),
                    source_persona: "merge_captain".to_string(),
                    target_persona_or_human: HandoffTargetRecord {
                        kind: "persona".to_string(),
                        id: Some("review_captain".to_string()),
                        label: Some("review_captain".to_string()),
                    },
                    task: "Review the patch".to_string(),
                    reason: "Merge queue requires review".to_string(),
                    ..Default::default()
                }
                .normalize(),
            ),
        });
        let line = rx.recv().await.expect("acp handoff notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(payload["method"], "session/update");
        assert_eq!(payload["params"]["update"]["sessionUpdate"], "handoff");
        assert_eq!(payload["params"]["update"]["handoffId"], "handoff-1");
        assert_eq!(
            payload["params"]["update"]["handoff"]["target_persona_or_human"]["label"],
            "review_captain"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn forwarded_agent_events_serialize_as_session_updates() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        let handoff = HandoffArtifact {
            id: "handoff-1".to_string(),
            source_persona: "merge_captain".to_string(),
            target_persona_or_human: HandoffTargetRecord {
                kind: "persona".to_string(),
                id: Some("review_captain".to_string()),
                label: Some("review_captain".to_string()),
            },
            task: "Review the patch".to_string(),
            reason: "Merge queue requires review".to_string(),
            ..Default::default()
        }
        .normalize();

        let events = vec![
            AgentEvent::AgentMessageChunk {
                session_id: "session-1".to_string(),
                content: "hello".to_string(),
            },
            AgentEvent::AgentThoughtChunk {
                session_id: "session-1".to_string(),
                content: "thinking".to_string(),
            },
            AgentEvent::ToolCall {
                session_id: "session-1".to_string(),
                tool_call_id: "tool-1".to_string(),
                tool_name: "read".to_string(),
                kind: Some(ToolKind::Read),
                status: ToolCallStatus::Pending,
                raw_input: serde_json::json!({"path": "README.md"}),
            },
            AgentEvent::ToolCallUpdate {
                session_id: "session-1".to_string(),
                tool_call_id: "tool-1".to_string(),
                tool_name: "read".to_string(),
                status: ToolCallStatus::Completed,
                raw_output: Some(serde_json::json!({"ok": true})),
                error: None,
                duration_ms: Some(7),
                execution_duration_ms: Some(5),
            },
            AgentEvent::Plan {
                session_id: "session-1".to_string(),
                plan: serde_json::json!([{"step": "edit", "status": "pending"}]),
            },
            AgentEvent::SkillActivated {
                session_id: "session-1".to_string(),
                skill_name: "rust".to_string(),
                iteration: 1,
                reason: "matched".to_string(),
            },
            AgentEvent::SkillDeactivated {
                session_id: "session-1".to_string(),
                skill_name: "rust".to_string(),
                iteration: 2,
            },
            AgentEvent::SkillScopeTools {
                session_id: "session-1".to_string(),
                skill_name: "rust".to_string(),
                allowed_tools: vec!["read".to_string()],
            },
            AgentEvent::ToolSearchQuery {
                session_id: "session-1".to_string(),
                tool_use_id: "search-1".to_string(),
                name: "tool_search".to_string(),
                query: serde_json::json!({"q": "read"}),
                strategy: "semantic".to_string(),
                mode: "client".to_string(),
            },
            AgentEvent::ToolSearchResult {
                session_id: "session-1".to_string(),
                tool_use_id: "search-1".to_string(),
                promoted: vec!["read".to_string()],
                strategy: "semantic".to_string(),
                mode: "client".to_string(),
            },
            AgentEvent::TranscriptCompacted {
                session_id: "session-1".to_string(),
                mode: "auto".to_string(),
                strategy: "summary".to_string(),
                archived_messages: 3,
                estimated_tokens_before: 100,
                estimated_tokens_after: 40,
                snapshot_asset_id: Some("asset-1".to_string()),
            },
            AgentEvent::Handoff {
                session_id: "session-1".to_string(),
                artifact_id: "artifact-1".to_string(),
                handoff: Box::new(handoff),
            },
            AgentEvent::FsWatch {
                session_id: "session-1".to_string(),
                subscription_id: "fsw-1".to_string(),
                events: vec![FsWatchEvent {
                    kind: "modify".to_string(),
                    paths: vec!["/tmp/project/src/lib.rs".to_string()],
                    relative_paths: vec!["src/lib.rs".to_string()],
                    raw_kind: "Modify(Any)".to_string(),
                    error: None,
                }],
            },
        ];
        let expected_updates = [
            "agent_message_chunk",
            "agent_thought_chunk",
            "tool_call",
            "tool_call_update",
            "plan",
            "skill_activated",
            "skill_deactivated",
            "skill_scope_tools",
            "tool_search_query",
            "tool_search_result",
            "transcript_compacted",
            "handoff",
            "fs_watch",
        ];

        for event in &events {
            sink.handle_event(event);
        }

        for expected in expected_updates {
            let line = rx.recv().await.expect("ACP event notification");
            let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
            assert_eq!(payload["method"], "session/update");
            assert_eq!(payload["params"]["sessionId"], "session-1");
            assert_eq!(payload["params"]["update"]["sessionUpdate"], expected);
        }
    }

    #[test]
    fn internal_agent_events_do_not_emit_session_updates() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

        sink.handle_event(&AgentEvent::TurnStart {
            session_id: "session-1".to_string(),
            iteration: 1,
        });
        sink.handle_event(&AgentEvent::BudgetExhausted {
            session_id: "session-1".to_string(),
            max_iterations: 3,
        });
        sink.handle_event(&AgentEvent::TurnEnd {
            session_id: "session-1".to_string(),
            iteration: 1,
            turn_info: serde_json::json!({}),
        });
        sink.handle_event(&AgentEvent::FeedbackInjected {
            session_id: "session-1".to_string(),
            kind: "user".to_string(),
            content: "continue".to_string(),
        });
        sink.handle_event(&AgentEvent::LoopStuck {
            session_id: "session-1".to_string(),
            max_nudges: 2,
            last_iteration: 4,
            tail_excerpt: "tail".to_string(),
        });
        sink.handle_event(&AgentEvent::DaemonWatchdogTripped {
            session_id: "session-1".to_string(),
            attempts: 3,
            elapsed_ms: 10,
        });

        assert!(rx.try_recv().is_err());
    }
}
