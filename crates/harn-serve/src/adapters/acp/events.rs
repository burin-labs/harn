//! ACP AgentEventSink — translates canonical `AgentEvent` variants into ACP
//! `session/update` notifications. Registered per-session at prompt start.

use harn_vm::agent_events::{AgentEvent, AgentEventSink, ToolExecutor};
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

    /// Render a `ToolExecutor` for the wire as either a bare string
    /// (unit variants) or `{"kind": "mcp_server", "serverName": "..."}`
    /// for the MCP variant. Matches harn#691's contract: clients can
    /// `typeof executor === "string"` first, then drill into `kind`.
    fn executor_to_json(executor: &ToolExecutor) -> serde_json::Value {
        match executor {
            ToolExecutor::HarnBuiltin => serde_json::Value::String("harn_builtin".to_string()),
            ToolExecutor::HostBridge => serde_json::Value::String("host_bridge".to_string()),
            ToolExecutor::McpServer { server_name } => serde_json::json!({
                "kind": "mcp_server",
                "serverName": server_name,
            }),
            ToolExecutor::ProviderNative => {
                serde_json::Value::String("provider_native".to_string())
            }
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
                error_category,
                executor,
                raw_input,
                raw_input_partial,
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
                if let Some(cat) = error_category {
                    update["errorCategory"] = serde_json::Value::String(cat.as_str().to_string());
                }
                if let Some(exec) = executor {
                    update["executor"] = Self::executor_to_json(exec);
                }
                // Streaming-only fields (harn#693). The two are
                // mutually exclusive: when the partial JSON parsed, the
                // structured value lands as `rawInput`; on parse
                // failure the raw bytes spill into `rawInputPartial`.
                // Clients keying off `rawInput` get a no-op on parse
                // failure and can fall back to the partial-bytes path.
                if let Some(input) = raw_input {
                    update["rawInput"] = input.clone();
                }
                if let Some(partial) = raw_input_partial {
                    update["rawInputPartial"] = serde_json::Value::String(partial.clone());
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
    use harn_vm::agent_events::{
        AgentEvent, AgentEventSink, FsWatchEvent, ToolCallErrorCategory, ToolCallStatus,
        ToolExecutor,
    };
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
                error_category: None,
                executor: Some(ToolExecutor::HarnBuiltin),
                raw_input: None,
                raw_input_partial: None,
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

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_update_serializes_error_category_in_camel_case() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-7".to_string(),
            tool_name: "read".to_string(),
            status: ToolCallStatus::Failed,
            raw_output: None,
            error: Some("missing required arg `path`".to_string()),
            duration_ms: None,
            execution_duration_ms: None,
            error_category: Some(ToolCallErrorCategory::SchemaValidation),
            executor: None,
            raw_input: None,
            raw_input_partial: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(
            payload["params"]["update"]["sessionUpdate"],
            "tool_call_update"
        );
        assert_eq!(payload["params"]["update"]["status"], "failed");
        assert_eq!(
            payload["params"]["update"]["errorCategory"],
            "schema_validation"
        );
        assert_eq!(
            payload["params"]["update"]["error"],
            "missing required arg `path`"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_update_omits_error_category_when_none() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-7".to_string(),
            tool_name: "read".to_string(),
            status: ToolCallStatus::Completed,
            raw_output: Some(serde_json::json!({"ok": true})),
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            executor: None,
            raw_input: None,
            raw_input_partial: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert!(payload["params"]["update"].get("errorCategory").is_none());
        assert!(payload["params"]["update"].get("error").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_update_serializes_executor_per_acp_wire_format() {
        // Harn#691: clients render badges off the ACP `executor` field.
        // The wire shape must distinguish bare-string variants from the
        // McpServer object-with-serverName form so a UI can branch on
        // `typeof executor === "string"`.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

        let cases = [
            (ToolExecutor::HarnBuiltin, serde_json::json!("harn_builtin")),
            (ToolExecutor::HostBridge, serde_json::json!("host_bridge")),
            (
                ToolExecutor::McpServer {
                    server_name: "linear".into(),
                },
                serde_json::json!({"kind": "mcp_server", "serverName": "linear"}),
            ),
            (
                ToolExecutor::ProviderNative,
                serde_json::json!("provider_native"),
            ),
        ];

        for (executor, expected) in cases {
            sink.handle_event(&AgentEvent::ToolCallUpdate {
                session_id: "session-1".to_string(),
                tool_call_id: "tool-1".to_string(),
                tool_name: "demo".to_string(),
                status: ToolCallStatus::Completed,
                raw_output: None,
                error: None,
                duration_ms: None,
                execution_duration_ms: None,
                error_category: None,
                executor: Some(executor),
                raw_input: None,
                raw_input_partial: None,
            });
            let line = rx.recv().await.expect("acp tool_call_update notification");
            let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
            assert_eq!(
                payload["params"]["update"]["sessionUpdate"],
                "tool_call_update"
            );
            assert_eq!(payload["params"]["update"]["executor"], expected);
        }

        // `executor: None` must not surface on the wire so existing
        // clients that don't know about the field aren't surprised.
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-2".to_string(),
            tool_name: "demo".to_string(),
            status: ToolCallStatus::InProgress,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            executor: None,
            raw_input: None,
            raw_input_partial: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert!(
            payload["params"]["update"].get("executor").is_none(),
            "got: {payload}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_update_serializes_raw_input_when_partial_parse_succeeded() {
        // harn#693: streaming `Pending` updates from the native-streaming
        // forwarder carry the structured value in `rawInput` whenever
        // the permissive partial-JSON parser succeeded. ACP clients can
        // render the live preview directly off `rawInput`.
        use harn_vm::agent_events::ToolCallStatus;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-toolu_42".to_string(),
            tool_name: "edit".to_string(),
            status: ToolCallStatus::Pending,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            executor: None,
            raw_input: Some(serde_json::json!({"path": "src/lib"})),
            raw_input_partial: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(payload["params"]["update"]["status"], "pending");
        assert_eq!(
            payload["params"]["update"]["rawInput"],
            serde_json::json!({"path": "src/lib"})
        );
        // Mutually exclusive with `rawInputPartial` — must be absent
        // when `rawInput` was set.
        assert!(
            payload["params"]["update"].get("rawInputPartial").is_none(),
            "got: {payload}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_update_serializes_raw_input_partial_when_partial_parse_failed() {
        use harn_vm::agent_events::ToolCallStatus;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-toolu_42".to_string(),
            tool_name: "edit".to_string(),
            status: ToolCallStatus::Pending,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            executor: None,
            raw_input: None,
            raw_input_partial: Some(r#"{"path": "a\"#.to_string()),
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(
            payload["params"]["update"]["rawInputPartial"],
            r#"{"path": "a\"#
        );
        assert!(payload["params"]["update"].get("rawInput").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_update_omits_streaming_fields_when_absent() {
        // Terminal updates (Completed/Failed) carry neither `rawInput`
        // nor `rawInputPartial`. The wire format must omit both keys
        // so legacy clients keying off presence aren't confused.
        use harn_vm::agent_events::ToolCallStatus;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-toolu_42".to_string(),
            tool_name: "edit".to_string(),
            status: ToolCallStatus::Completed,
            raw_output: Some(serde_json::json!({"ok": true})),
            error: None,
            duration_ms: Some(7),
            execution_duration_ms: Some(5),
            error_category: None,
            executor: None,
            raw_input: None,
            raw_input_partial: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert!(payload["params"]["update"].get("rawInput").is_none());
        assert!(payload["params"]["update"].get("rawInputPartial").is_none());
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
