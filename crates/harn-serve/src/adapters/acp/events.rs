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
                parsing,
                audit,
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
                if let Some(p) = parsing {
                    update["parsing"] = serde_json::Value::Bool(*p);
                }
                if let Some(record) = audit {
                    if let Ok(value) = serde_json::to_value(record) {
                        update["audit"] = value;
                    }
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
                parsing,
                raw_input,
                raw_input_partial,
                audit,
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
                if let Some(p) = parsing {
                    update["parsing"] = serde_json::Value::Bool(*p);
                }
                if let Some(record) = audit {
                    if let Ok(value) = serde_json::to_value(record) {
                        update["audit"] = value;
                    }
                }
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
                let entries = if plan
                    .get("schema_version")
                    .and_then(serde_json::Value::as_str)
                    == Some(harn_vm::llm::plan::PLAN_SCHEMA_VERSION)
                {
                    harn_vm::llm::plan::plan_entries(plan)
                } else {
                    plan.clone()
                };
                let mut update = serde_json::json!({
                    "sessionUpdate": "plan",
                    "entries": entries,
                });
                if plan
                    .get("schema_version")
                    .and_then(serde_json::Value::as_str)
                    == Some(harn_vm::llm::plan::PLAN_SCHEMA_VERSION)
                {
                    update["harnPlan"] = plan.clone();
                }
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
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
            AgentEvent::WorkerUpdate {
                session_id,
                worker_id,
                worker_name,
                worker_task,
                worker_mode,
                event,
                status,
                metadata,
                audit,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "worker_update",
                    "workerId": worker_id,
                    "workerName": worker_name,
                    "workerTask": worker_task,
                    "workerMode": worker_mode,
                    "event": event.as_str(),
                    "status": status,
                    "terminal": event.is_terminal(),
                    "metadata": metadata,
                });
                if let Some(audit) = audit {
                    update["audit"] = audit.clone();
                }
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
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
    use harn_vm::orchestration::{
        HandoffArtifact, HandoffTargetRecord, MutationSessionRecord, ToolApprovalPolicy,
    };
    use harn_vm::tool_annotations::ToolKind;
    use tokio::sync::mpsc;

    use super::super::HARN_SESSION_UPDATE_EXTENSIONS;
    use super::{AcpAgentEventSink, AcpOutput};

    const ACP_V0_12_2_SESSION_UPDATES: &[&str] = &[
        "user_message_chunk",
        "agent_message_chunk",
        "agent_thought_chunk",
        "tool_call",
        "tool_call_update",
        "plan",
        "available_commands_update",
        "current_mode_update",
        "config_option_update",
        "session_info_update",
    ];

    async fn collect_notifications(events: Vec<AgentEvent>) -> Vec<serde_json::Value> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        let expected_len = events.len();
        for event in events {
            sink.handle_event(&event);
        }

        let mut notifications = Vec::with_capacity(expected_len);
        for _ in 0..expected_len {
            let line = rx.recv().await.expect("ACP event notification");
            notifications.push(serde_json::from_str(&line).expect("json"));
        }
        notifications
    }

    fn fixture_handoff() -> HandoffArtifact {
        HandoffArtifact {
            type_name: "handoff_artifact".to_string(),
            id: "handoff-1".to_string(),
            parent_run_id: None,
            source_persona: "merge_captain".to_string(),
            target_persona_or_human: HandoffTargetRecord {
                kind: "persona".to_string(),
                id: Some("review_captain".to_string()),
                label: Some("review_captain".to_string()),
            },
            task: "Review the patch".to_string(),
            reason: "Merge queue requires review".to_string(),
            created_at: "2026-04-28T00:00:00Z".to_string(),
            ..Default::default()
        }
    }

    fn standard_fixture_events() -> Vec<AgentEvent> {
        vec![
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
                parsing: None,
                audit: None,
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
                parsing: None,
                raw_input: None,
                raw_input_partial: None,
                audit: None,
            },
            AgentEvent::Plan {
                session_id: "session-1".to_string(),
                plan: serde_json::json!([
                    {"content": "edit", "status": "pending"}
                ]),
            },
        ]
    }

    fn extension_fixture_events() -> Vec<AgentEvent> {
        vec![
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
                handoff: Box::new(fixture_handoff()),
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
            AgentEvent::WorkerUpdate {
                session_id: "session-1".into(),
                worker_id: "worker-1".into(),
                worker_name: "review".into(),
                worker_task: "review pr".into(),
                worker_mode: "delegated_stage".into(),
                event: harn_vm::agent_events::WorkerEvent::WorkerWaitingForInput,
                status: "awaiting_input".into(),
                metadata: serde_json::json!({
                    "child_run_id": "run_x",
                    "child_run_path": ".harn-runs/run_x",
                }),
                audit: Some(serde_json::json!({"run_id": "run_x"})),
            },
        ]
    }

    #[tokio::test(flavor = "current_thread")]
    async fn standard_session_update_fixtures_match_acp_schema_v0_12_2_discriminators() {
        let actual = collect_notifications(standard_fixture_events()).await;
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/acp/session_update_standard.json"
        ))
        .expect("fixture json");
        assert_eq!(serde_json::Value::Array(actual.clone()), expected);

        for notification in actual {
            let session_update = notification["params"]["update"]["sessionUpdate"]
                .as_str()
                .expect("sessionUpdate");
            assert!(
                ACP_V0_12_2_SESSION_UPDATES.contains(&session_update),
                "{session_update} is not a standard ACP v0.12.2 SessionUpdate"
            );
            if session_update == "plan" {
                assert!(notification["params"]["update"].get("entries").is_some());
                assert!(notification["params"]["update"].get("plan").is_none());
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn harn_extension_session_update_fixtures_are_pinned() {
        let actual = collect_notifications(extension_fixture_events()).await;
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/acp/session_update_extensions.json"
        ))
        .expect("fixture json");
        assert_eq!(serde_json::Value::Array(actual.clone()), expected);

        for notification in actual {
            let session_update = notification["params"]["update"]["sessionUpdate"]
                .as_str()
                .expect("sessionUpdate");
            assert!(
                HARN_SESSION_UPDATE_EXTENSIONS.contains(&session_update),
                "{session_update} is not advertised as a Harn ACP extension"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn structured_plan_extension_fixture_is_pinned() {
        let plan = harn_vm::llm::plan::normalize_plan_tool_call(
            harn_vm::llm::plan::EMIT_PLAN_TOOL,
            &serde_json::json!({
                "summary": "Ship plan events.",
                "steps": [
                    {"content": "Emit plan event.", "status": "completed"},
                    {"content": "Verify fixtures.", "status": "pending"}
                ],
                "verification_commands": ["cargo test -p harn-serve acp"],
            }),
        );
        let actual = collect_notifications(vec![AgentEvent::Plan {
            session_id: "session-1".to_string(),
            plan,
        }])
        .await;
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/acp/session_update_plan_extension.json"
        ))
        .expect("fixture json");
        assert_eq!(serde_json::Value::Array(actual), expected);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worker_update_serializes_to_session_update_with_lifecycle_metadata() {
        // Every typed `WorkerEvent` must round-trip onto the ACP
        // `session/update` stream as a `worker_update` entry. The
        // adapter pins a stable wire shape: status string, event
        // discriminator, terminal hint, plus the structured metadata
        // and audit fields hosts render without re-parsing.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

        let cases = [
            (
                harn_vm::agent_events::WorkerEvent::WorkerSpawned,
                "running",
                false,
            ),
            (
                harn_vm::agent_events::WorkerEvent::WorkerProgressed,
                "progressed",
                false,
            ),
            (
                harn_vm::agent_events::WorkerEvent::WorkerWaitingForInput,
                "awaiting_input",
                false,
            ),
            (
                harn_vm::agent_events::WorkerEvent::WorkerCompleted,
                "completed",
                true,
            ),
            (
                harn_vm::agent_events::WorkerEvent::WorkerFailed,
                "failed",
                true,
            ),
            (
                harn_vm::agent_events::WorkerEvent::WorkerCancelled,
                "cancelled",
                true,
            ),
        ];

        for (worker_event, status, terminal) in cases {
            sink.handle_event(&AgentEvent::WorkerUpdate {
                session_id: "session-1".into(),
                worker_id: "worker-1".into(),
                worker_name: "review".into(),
                worker_task: "review pr".into(),
                worker_mode: "delegated_stage".into(),
                event: worker_event,
                status: worker_event.as_status().to_string(),
                metadata: serde_json::json!({
                    "child_run_id": "run_x",
                    "child_run_path": ".harn-runs/run_x",
                }),
                audit: Some(serde_json::json!({"run_id": "run_x"})),
            });
            let line = rx.recv().await.expect("acp worker_update notification");
            let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
            assert_eq!(payload["method"], "session/update");
            assert_eq!(payload["params"]["sessionId"], "session-1");
            let update = &payload["params"]["update"];
            assert_eq!(update["sessionUpdate"], "worker_update");
            assert_eq!(update["workerId"], "worker-1");
            assert_eq!(update["workerName"], "review");
            assert_eq!(update["workerTask"], "review pr");
            assert_eq!(update["workerMode"], "delegated_stage");
            assert_eq!(update["event"], worker_event.as_str());
            assert_eq!(update["status"], status);
            assert_eq!(update["terminal"], terminal);
            assert_eq!(update["metadata"]["child_run_id"], "run_x");
            assert_eq!(update["audit"]["run_id"], "run_x");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worker_update_omits_audit_when_absent() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        sink.handle_event(&AgentEvent::WorkerUpdate {
            session_id: "session-1".into(),
            worker_id: "w".into(),
            worker_name: "n".into(),
            worker_task: "t".into(),
            worker_mode: "delegated_stage".into(),
            event: harn_vm::agent_events::WorkerEvent::WorkerSpawned,
            status: "running".into(),
            metadata: serde_json::json!({}),
            audit: None,
        });
        let line = rx.recv().await.expect("acp worker_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert!(payload["params"]["update"].get("audit").is_none());
    }

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
                parsing: None,
                audit: None,
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
                parsing: None,

                raw_input: None,
                raw_input_partial: None,
                audit: None,
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
            parsing: None,

            raw_input: None,
            raw_input_partial: None,
            audit: None,
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
            parsing: None,

            raw_input: None,
            raw_input_partial: None,
            audit: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert!(payload["params"]["update"].get("errorCategory").is_none());
        assert!(payload["params"]["update"].get("error").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_carries_parsing_flag_through_to_acp_wire() {
        // Harn#692: when the streaming candidate detector emits a
        // tool_call with `parsing: Some(true)`, the ACP wire must carry
        // a literal `"parsing": true` so clients can render the
        // in-flight chip. The terminal `tool_call_update` likewise
        // carries `"parsing": false` to retract it.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

        sink.handle_event(&AgentEvent::ToolCall {
            session_id: "session-1".to_string(),
            tool_call_id: "text-cand-1".to_string(),
            tool_name: "edit".to_string(),
            kind: None,
            status: ToolCallStatus::Pending,
            raw_input: serde_json::json!({}),
            parsing: Some(true),
            audit: None,
        });
        let line = rx.recv().await.expect("acp tool_call notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(payload["params"]["update"]["sessionUpdate"], "tool_call");
        assert_eq!(payload["params"]["update"]["parsing"], true);

        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "text-cand-1".to_string(),
            tool_name: "edit".to_string(),
            status: ToolCallStatus::Failed,
            raw_output: None,
            error: Some("malformed args".to_string()),
            duration_ms: None,
            execution_duration_ms: None,
            error_category: Some(ToolCallErrorCategory::ParseAborted),
            executor: None,
            parsing: Some(false),

            raw_input: None,

            raw_input_partial: None,
            audit: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(
            payload["params"]["update"]["sessionUpdate"],
            "tool_call_update"
        );
        assert_eq!(payload["params"]["update"]["parsing"], false);
        assert_eq!(
            payload["params"]["update"]["errorCategory"],
            "parse_aborted"
        );

        // Default `parsing: None` must not surface a `parsing` field
        // at all, so existing ACP clients that don't know about the
        // candidate phase don't see a misleading `null`.
        sink.handle_event(&AgentEvent::ToolCall {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "read".to_string(),
            kind: None,
            status: ToolCallStatus::Pending,
            raw_input: serde_json::json!({}),
            parsing: None,
            audit: None,
        });
        let line = rx.recv().await.expect("acp tool_call notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert!(
            payload["params"]["update"].get("parsing").is_none(),
            "got: {payload}"
        );
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
                parsing: None,

                raw_input: None,
                raw_input_partial: None,
                audit: None,
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
            parsing: None,

            raw_input: None,
            raw_input_partial: None,
            audit: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert!(
            payload["params"]["update"].get("executor").is_none(),
            "got: {payload}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_update_streams_raw_input_and_raw_input_partial_per_acp_wire_format() {
        // #693: ACP wire format must mirror raw_input as `rawInput` and
        // raw_input_partial as `rawInputPartial`. Both fields skip
        // serialization when None so older clients see no surprise
        // keys.
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

        // Parsed partial value → `rawInput` populated, `rawInputPartial` absent.
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-streaming".to_string(),
            tool_name: "search".to_string(),
            status: ToolCallStatus::Pending,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            executor: None,
            raw_input: Some(serde_json::json!({"q": "hello"})),
            raw_input_partial: None,
            audit: None,

            parsing: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(payload["params"]["update"]["rawInput"]["q"], "hello");
        assert!(payload["params"]["update"].get("rawInputPartial").is_none());

        // Unparseable partial bytes → `rawInputPartial` populated, `rawInput` absent.
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-streaming".to_string(),
            tool_name: "search".to_string(),
            status: ToolCallStatus::Pending,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            executor: None,
            parsing: None,
            raw_input: None,
            raw_input_partial: Some(r#"{"q":"hel"#.to_string()),
            audit: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert!(payload["params"]["update"].get("rawInput").is_none());
        assert_eq!(
            payload["params"]["update"]["rawInputPartial"],
            r#"{"q":"hel"#
        );

        // Terminal updates (None / None) must not introduce these keys.
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-streaming".to_string(),
            tool_name: "search".to_string(),
            status: ToolCallStatus::Completed,
            raw_output: Some(serde_json::json!({"ok": true})),
            error: None,
            duration_ms: Some(12),
            execution_duration_ms: Some(8),
            error_category: None,
            executor: None,
            parsing: None,
            raw_input: None,
            raw_input_partial: None,
            audit: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert!(payload["params"]["update"].get("rawInput").is_none());
        assert!(payload["params"]["update"].get("rawInputPartial").is_none());
        assert_eq!(payload["params"]["update"]["status"], "completed");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_includes_audit_when_mutation_session_is_active() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        let policy = ToolApprovalPolicy {
            require_approval: vec!["edit_*".into()],
            write_path_allowlist: vec!["src/**".into()],
            ..Default::default()
        };
        let audit = MutationSessionRecord {
            session_id: "session_42".into(),
            parent_session_id: Some("session_root".into()),
            run_id: Some("run_42".into()),
            worker_id: Some("worker_3".into()),
            execution_kind: Some("worker".into()),
            mutation_scope: "apply_workspace".into(),
            approval_policy: Some(policy),
        };
        sink.handle_event(&AgentEvent::ToolCall {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "edit_file".to_string(),
            kind: None,
            status: ToolCallStatus::Pending,
            raw_input: serde_json::json!({"path": "src/main.rs"}),
            parsing: None,
            audit: Some(audit),
        });
        let line = rx.recv().await.expect("acp tool_call notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        let audit_value = &payload["params"]["update"]["audit"];
        assert_eq!(audit_value["session_id"], "session_42");
        assert_eq!(audit_value["parent_session_id"], "session_root");
        assert_eq!(audit_value["run_id"], "run_42");
        assert_eq!(audit_value["worker_id"], "worker_3");
        assert_eq!(audit_value["execution_kind"], "worker");
        assert_eq!(audit_value["mutation_scope"], "apply_workspace");
        assert_eq!(
            audit_value["approval_policy"]["require_approval"][0],
            "edit_*"
        );
        assert_eq!(
            audit_value["approval_policy"]["write_path_allowlist"][0],
            "src/**"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_omits_audit_when_no_mutation_session() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        sink.handle_event(&AgentEvent::ToolCall {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "read".to_string(),
            kind: Some(ToolKind::Read),
            status: ToolCallStatus::Pending,
            raw_input: serde_json::json!({"path": "README.md"}),
            parsing: None,
            audit: None,
        });
        let line = rx.recv().await.expect("acp tool_call notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert!(
            payload["params"]["update"].get("audit").is_none(),
            "got: {payload}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_update_includes_audit_when_mutation_session_is_active() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        let audit = MutationSessionRecord {
            session_id: "session_42".into(),
            run_id: Some("run_42".into()),
            mutation_scope: "apply_workspace".into(),
            execution_kind: Some("workflow".into()),
            ..Default::default()
        };
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "edit_file".to_string(),
            status: ToolCallStatus::Completed,
            raw_output: Some(serde_json::json!({"text": "ok"})),
            error: None,
            duration_ms: Some(11),
            execution_duration_ms: Some(7),
            error_category: None,
            executor: Some(ToolExecutor::HostBridge),
            parsing: None,
            raw_input: None,
            raw_input_partial: None,
            audit: Some(audit),
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        let update = &payload["params"]["update"];
        assert_eq!(update["sessionUpdate"], "tool_call_update");
        assert_eq!(update["audit"]["session_id"], "session_42");
        assert_eq!(update["audit"]["run_id"], "run_42");
        assert_eq!(update["audit"]["mutation_scope"], "apply_workspace");
        assert_eq!(update["audit"]["execution_kind"], "workflow");
        assert_eq!(update["executor"], "host_bridge");
        assert_eq!(update["durationMs"], 11);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_update_omits_audit_when_no_mutation_session() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "read".to_string(),
            status: ToolCallStatus::Completed,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            executor: None,
            parsing: None,
            raw_input: None,
            raw_input_partial: None,
            audit: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert!(
            payload["params"]["update"].get("audit").is_none(),
            "got: {payload}"
        );
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
