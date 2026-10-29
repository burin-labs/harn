//! ACP AgentEventSink — translates canonical `AgentEvent` variants into ACP
//! `session/update` notifications. Registered per-session at prompt start.

use super::agent_event_payloads::{self as payloads, AgentEventPayload};
use super::AcpOutput;
use harn_vm::agent_events::{AgentEvent, AgentEventSink, ToolExecutor};
use harn_vm::visible_text::sanitize_visible_assistant_text;

/// Writes canonical ACP `session/update` notifications for each `AgentEvent`.
pub(super) struct AcpAgentEventSink {
    output: AcpOutput,
    replayed: bool,
    hitl_log: Option<super::prompt_owner::PromptExecutionOwner>,
    receipts: Option<super::delivery::PromptReceipts>,
}

impl AcpAgentEventSink {
    pub(super) fn new(output: AcpOutput) -> Self {
        Self {
            output,
            replayed: false,
            hitl_log: None,
            receipts: None,
        }
    }

    pub(super) fn for_prompt(
        output: AcpOutput,
        hitl_log: super::prompt_owner::PromptExecutionOwner,
        receipts: super::delivery::PromptReceipts,
    ) -> Self {
        Self {
            output,
            replayed: false,
            hitl_log: Some(hitl_log),
            receipts: Some(receipts),
        }
    }

    pub(super) fn for_replay(output: AcpOutput) -> Self {
        Self {
            output,
            replayed: true,
            hitl_log: None,
            receipts: None,
        }
    }

    fn write_notification(&self, params: serde_json::Value) {
        self.write_jsonrpc_notification("session/update", params);
    }

    /// Emit a JSON-RPC notification through the `session/update` transport.
    /// Used for vendor-prefixed ACP `ExtNotification` envelopes when the
    /// canonical discriminator has no slot for `_harn/agentEvent`.
    fn write_jsonrpc_notification(&self, method: &str, mut params: serde_json::Value) {
        if self.replayed {
            mark_replayed_params(method, &mut params);
        }
        let mut notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        if self.replayed {
            notification["_harn"] = serde_json::json!({"replayed": true});
        }
        if let Ok(line) = serde_json::to_string(&notification) {
            self.output.write_line(&line);
        }
    }

    /// Emit a Harn agent event as `_harn/agentEvent`: top-level `sessionId`
    /// and `kind`, plus the kind's typed body directly under `params`.
    fn emit_agent_event<P: AgentEventPayload>(&self, session_id: &str, payload: &P) {
        let body = serde_json::to_value(payload).expect("agent event payload serializes");
        let params = agent_event_ext_params(P::KIND, session_id, body)
            .unwrap_or_else(|error| panic!("{error}"));
        self.write_jsonrpc_notification(super::schema::HARN_AGENT_EVENT_METHOD, params);
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

    fn attach_harn_meta(
        update: &mut serde_json::Value,
        harn_meta: serde_json::Map<String, serde_json::Value>,
    ) {
        merge_harn_meta(update, harn_meta);
    }
}

/// A nested runtime record carried as an untyped field of an event body.
fn to_wire<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).expect("agent event field serializes")
}

fn agent_event_ext_params(
    kind: &str,
    session_id: &str,
    mut payload: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let obj = payload
        .as_object_mut()
        .ok_or_else(|| format!("agent event {kind:?} payload must be a JSON object"))?;
    let collisions = ["kind", "sessionId"]
        .into_iter()
        .filter(|key| obj.contains_key(*key))
        .collect::<Vec<_>>();
    if !collisions.is_empty() {
        return Err(format!(
            "agent event {kind:?} payload uses reserved envelope key(s): {}",
            collisions.join(", ")
        ));
    }
    obj.insert(
        "sessionId".to_string(),
        serde_json::Value::String(session_id.to_string()),
    );
    obj.insert(
        "kind".to_string(),
        serde_json::Value::String(kind.to_string()),
    );
    Ok(payload)
}

fn mark_replayed_params(method: &str, params: &mut serde_json::Value) {
    if method == "session/update" {
        if let Some(update) = params.get_mut("update") {
            let mut harn_meta = serde_json::Map::new();
            harn_meta.insert("replayed".to_string(), serde_json::Value::Bool(true));
            merge_harn_meta(update, harn_meta);
        }
        return;
    }
    if method.starts_with('_') {
        if let Some(obj) = params.as_object_mut() {
            obj.insert("replayed".to_string(), serde_json::Value::Bool(true));
        }
    }
}

impl AgentEventSink for AcpAgentEventSink {
    fn handle_event(&self, event: &AgentEvent) {
        match event {
            // Handler diagnostics that carry the whole record their stdlib
            // emitter built.
            AgentEvent::CacheHit {
                session_id,
                payload,
                ..
            } => {
                self.emit_agent_event(session_id, &payloads::CacheHit::from_value(payload.clone()));
            }
            AgentEvent::CacheMiss {
                session_id,
                payload,
                ..
            } => self.emit_agent_event(
                session_id,
                &payloads::CacheMiss::from_value(payload.clone()),
            ),
            AgentEvent::LlmCallLog {
                session_id,
                payload,
                ..
            } => self.emit_agent_event(
                session_id,
                &payloads::LlmCallLog::from_value(payload.clone()),
            ),
            AgentEvent::LlmRoutingDecision {
                session_id,
                payload,
                ..
            } => self.emit_agent_event(
                session_id,
                &payloads::LlmRoutingDecision::from_value(payload.clone()),
            ),
            AgentEvent::LlmFallbackAttempt {
                session_id,
                payload,
                ..
            } => self.emit_agent_event(
                session_id,
                &payloads::LlmFallbackAttempt::from_value(payload.clone()),
            ),
            AgentEvent::LlmShadowDiff {
                session_id,
                payload,
                ..
            } => self.emit_agent_event(
                session_id,
                &payloads::LlmShadowDiff::from_value(payload.clone()),
            ),
            AgentEvent::SemanticCacheHit {
                session_id,
                payload,
                ..
            } => self.emit_agent_event(
                session_id,
                &payloads::SemanticCacheHit::from_value(payload.clone()),
            ),
            AgentEvent::SemanticCacheMiss {
                session_id,
                payload,
                ..
            } => self.emit_agent_event(
                session_id,
                &payloads::SemanticCacheMiss::from_value(payload.clone()),
            ),
            AgentEvent::AgentMessageChunk {
                session_id,
                content,
                history_source_event_id,
            } => {
                let visible = sanitize_visible_assistant_text(content, true);
                let mut content_block = serde_json::json!({
                    "type": "text",
                    "text": content,
                });
                let mut content_meta = serde_json::Map::new();
                content_meta.insert(
                    "visible_text".to_string(),
                    serde_json::Value::String(visible.clone()),
                );
                content_meta.insert(
                    "visible_delta".to_string(),
                    serde_json::Value::String(visible),
                );
                merge_harn_meta(&mut content_block, content_meta);
                let mut update = serde_json::json!({
                    "sessionUpdate": "agent_message_chunk",
                    "content": content_block,
                });
                if let Some(identity) = history_source_event_id {
                    update["historySourceEventId"] = serde_json::json!(identity);
                }
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
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
            AgentEvent::UserMessage {
                session_id,
                message_id,
                content,
                history_source_event_id,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "user_message",
                    "messageId": message_id,
                    "content": content,
                });
                if let Some(identity) = history_source_event_id {
                    update["historySourceEventId"] = serde_json::json!(identity);
                }
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
                if let Some(receipts) = &self.receipts {
                    receipts.inserted(session_id, message_id);
                }
            }
            AgentEvent::ToolCall { .. } | AgentEvent::ToolCallUpdate { .. } => {
                tool_lifecycle::handle(self, event);
            }
            AgentEvent::PlanDocumentUpdated { session_id, event } => {
                let document = event.document();
                let mut update = super::bridge::acp_plan_update(
                    harn_vm::llm::plan::plan_document_entries(document),
                );
                update["harnPlanDocument"] = serde_json::json!(document);
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
                let mut update = serde_json::json!({
                    "sessionUpdate": "skill_activated",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "skillName".to_string(),
                    serde_json::Value::String(skill_name.clone()),
                );
                harn_meta.insert("iteration".to_string(), serde_json::Value::from(*iteration));
                harn_meta.insert(
                    "reason".to_string(),
                    serde_json::Value::String(reason.clone()),
                );
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::SkillDeactivated {
                session_id,
                skill_name,
                iteration,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "skill_deactivated",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "skillName".to_string(),
                    serde_json::Value::String(skill_name.clone()),
                );
                harn_meta.insert("iteration".to_string(), serde_json::Value::from(*iteration));
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::SkillScopeTools {
                session_id,
                skill_name,
                allowed_tools,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "skill_scope_tools",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "skillName".to_string(),
                    serde_json::Value::String(skill_name.clone()),
                );
                harn_meta.insert(
                    "allowedTools".to_string(),
                    serde_json::to_value(allowed_tools).unwrap_or_default(),
                );
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::SkillNarrow {
                session_id,
                reason,
                removed_tools,
                remaining_tools,
                policy,
                removed_tool_details,
                kept_tool_details,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "skill_narrow",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "reason".to_string(),
                    serde_json::Value::String(reason.clone()),
                );
                harn_meta.insert(
                    "removedTools".to_string(),
                    serde_json::to_value(removed_tools).unwrap_or_default(),
                );
                harn_meta.insert(
                    "remainingTools".to_string(),
                    serde_json::to_value(remaining_tools).unwrap_or_default(),
                );
                if !policy.is_null() {
                    harn_meta.insert("policy".to_string(), policy.clone());
                }
                if !removed_tool_details.is_null() {
                    harn_meta.insert(
                        "removedToolDetails".to_string(),
                        removed_tool_details.clone(),
                    );
                }
                if !kept_tool_details.is_null() {
                    harn_meta.insert("keptToolDetails".to_string(), kept_tool_details.clone());
                }
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::StanceTransition {
                session_id,
                phase,
                escape_tool,
                allowed_tools,
                justification,
                consent,
                reason,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "stance_transition",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "phase".to_string(),
                    serde_json::Value::String(phase.clone()),
                );
                harn_meta.insert(
                    "escapeTool".to_string(),
                    serde_json::Value::String(escape_tool.clone()),
                );
                if !allowed_tools.is_empty() {
                    harn_meta.insert(
                        "allowedTools".to_string(),
                        serde_json::to_value(allowed_tools).unwrap_or_default(),
                    );
                }
                if !justification.is_empty() {
                    harn_meta.insert(
                        "justification".to_string(),
                        serde_json::Value::String(justification.clone()),
                    );
                }
                if !consent.is_empty() {
                    harn_meta.insert(
                        "consent".to_string(),
                        serde_json::Value::String(consent.clone()),
                    );
                }
                if !reason.is_empty() {
                    harn_meta.insert(
                        "reason".to_string(),
                        serde_json::Value::String(reason.clone()),
                    );
                }
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
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
                let mut update = serde_json::json!({
                    "sessionUpdate": "tool_search_query",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "toolUseId".to_string(),
                    serde_json::Value::String(tool_use_id.clone()),
                );
                harn_meta.insert("name".to_string(), serde_json::Value::String(name.clone()));
                harn_meta.insert("query".to_string(), query.clone());
                harn_meta.insert(
                    "strategy".to_string(),
                    serde_json::Value::String(strategy.clone()),
                );
                harn_meta.insert("mode".to_string(), serde_json::Value::String(mode.clone()));
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::ToolSearchResult {
                session_id,
                tool_use_id,
                promoted,
                strategy,
                mode,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "tool_search_result",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "toolUseId".to_string(),
                    serde_json::Value::String(tool_use_id.clone()),
                );
                harn_meta.insert(
                    "promoted".to_string(),
                    serde_json::to_value(promoted).unwrap_or_default(),
                );
                harn_meta.insert(
                    "strategy".to_string(),
                    serde_json::Value::String(strategy.clone()),
                );
                harn_meta.insert("mode".to_string(), serde_json::Value::String(mode.clone()));
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::TranscriptCompacted {
                session_id,
                receipt,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "transcript_compacted",
                });
                // The receipt owns its ACP projection (`receiptId` +
                // `schemaVersion` give consumers the stable runtime identity,
                // harn#4995; `requestedStrategy` + `sourceMeasurement` make a
                // requested-versus-applied divergence legible, harn#8531).
                let harn_meta = receipt.to_acp_meta();
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::TranscriptProjected {
                session_id,
                policy,
                reason,
                prefix_hash,
                kept_count,
                dropped_count,
                provider_safety_blocked,
                redacted_count,
                reclaimed_tokens,
                roots_consulted,
                redaction_pointers,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "transcript_projected",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "policy".to_string(),
                    serde_json::Value::String(policy.clone()),
                );
                harn_meta.insert(
                    "reason".to_string(),
                    serde_json::Value::String(reason.clone()),
                );
                harn_meta.insert(
                    "prefixHash".to_string(),
                    serde_json::Value::String(prefix_hash.clone()),
                );
                harn_meta.insert(
                    "keptCount".to_string(),
                    serde_json::Value::from(*kept_count),
                );
                harn_meta.insert(
                    "droppedCount".to_string(),
                    serde_json::Value::from(*dropped_count),
                );
                harn_meta.insert(
                    "providerSafetyBlocked".to_string(),
                    serde_json::Value::Bool(*provider_safety_blocked),
                );
                if *redacted_count > 0 {
                    harn_meta.insert(
                        "redactedCount".to_string(),
                        serde_json::Value::from(*redacted_count),
                    );
                }
                if *reclaimed_tokens > 0 {
                    harn_meta.insert(
                        "reclaimedTokens".to_string(),
                        serde_json::Value::from(*reclaimed_tokens),
                    );
                }
                if !roots_consulted.is_empty() {
                    harn_meta.insert(
                        "rootsConsulted".to_string(),
                        serde_json::to_value(roots_consulted).unwrap_or_default(),
                    );
                }
                if !redaction_pointers.is_empty() {
                    harn_meta.insert(
                        "redactionPointers".to_string(),
                        serde_json::Value::Array(redaction_pointers.clone()),
                    );
                }
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::ReminderEmitted { .. } => {
                self.write_notification(meta::reminder_notification(event));
            }
            AgentEvent::Handoff {
                session_id,
                artifact_id,
                handoff,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "handoff",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "handoffId".to_string(),
                    serde_json::Value::String(handoff.id.clone()),
                );
                harn_meta.insert(
                    "artifactId".to_string(),
                    serde_json::Value::String(artifact_id.clone()),
                );
                harn_meta.insert(
                    "handoff".to_string(),
                    serde_json::to_value(handoff).unwrap_or_default(),
                );
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::FsWatch {
                session_id,
                subscription_id,
                events,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "fs_watch",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "subscriptionId".to_string(),
                    serde_json::Value::String(subscription_id.clone()),
                );
                harn_meta.insert(
                    "events".to_string(),
                    serde_json::to_value(events).unwrap_or_default(),
                );
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::StagedWritesPending {
                session_id,
                pending_count,
                total_bytes,
                pending_writes,
            } => {
                let mut update = super::bridge::progress_update(
                    "fs_staging",
                    "staged writes pending",
                    Some(*pending_count as i64),
                    None,
                    None,
                );
                merge_harn_meta(
                    &mut update,
                    super::staged_writes::harn_meta(*pending_count, *total_bytes, pending_writes),
                );
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::SafeTextPatchResult {
                session_id,
                path,
                result,
                hunks_count,
                bytes_written,
                failed_hunk_index,
            } => {
                let mut update = super::bridge::progress_update(
                    "safe_text_patch",
                    &format!("safe_text_patch: {result}"),
                    None,
                    None,
                    None,
                );
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "kind".to_string(),
                    serde_json::Value::String("safe_text_patch_result".to_string()),
                );
                harn_meta.insert("path".to_string(), serde_json::Value::String(path.clone()));
                harn_meta.insert(
                    "result".to_string(),
                    serde_json::Value::String(result.clone()),
                );
                harn_meta.insert(
                    "hunksCount".to_string(),
                    serde_json::Value::from(*hunks_count as u64),
                );
                harn_meta.insert(
                    "bytesWritten".to_string(),
                    serde_json::Value::from(*bytes_written),
                );
                if let Some(idx) = failed_hunk_index {
                    harn_meta.insert(
                        "failedHunkIndex".to_string(),
                        serde_json::Value::from(*idx as u64),
                    );
                }
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::ControlOutcome {
                session_id,
                control_id,
                method,
                outcome,
                status,
                actor,
                target,
                reason,
                metadata,
            } => {
                self.emit_agent_event(
                    session_id,
                    &payloads::ControlOutcome {
                        control_id: control_id.clone(),
                        method: method.clone(),
                        outcome: outcome.clone(),
                        status: status.clone(),
                        actor: actor.clone(),
                        target: target.clone(),
                        reason: reason.clone(),
                        metadata: (!metadata.is_null()).then(|| metadata.clone()),
                    },
                );
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
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "workerId".to_string(),
                    serde_json::Value::String(worker_id.clone()),
                );
                harn_meta.insert(
                    "workerName".to_string(),
                    serde_json::Value::String(worker_name.clone()),
                );
                harn_meta.insert(
                    "workerTask".to_string(),
                    serde_json::Value::String(worker_task.clone()),
                );
                harn_meta.insert(
                    "workerMode".to_string(),
                    serde_json::Value::String(worker_mode.clone()),
                );
                harn_meta.insert(
                    "event".to_string(),
                    serde_json::Value::String(event.as_str().to_string()),
                );
                harn_meta.insert(
                    "status".to_string(),
                    serde_json::Value::String(status.clone()),
                );
                harn_meta.insert(
                    "terminal".to_string(),
                    serde_json::Value::Bool(event.is_terminal()),
                );
                harn_meta.insert("metadata".to_string(), metadata.clone());
                if let Some(audit) = audit {
                    harn_meta.insert("audit".to_string(), audit.clone());
                }
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::HitlRequested {
                session_id,
                request_id,
                kind,
                payload,
            } => {
                if let Some(log) = &self.hitl_log {
                    // HITL emits this only after creating the request and its
                    // waitpoint. Cached turns can create their log lazily.
                    log.capture_current();
                }
                let mut update = serde_json::json!({
                    "sessionUpdate": "hitl_request",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "requestId".to_string(),
                    serde_json::Value::String(request_id.clone()),
                );
                harn_meta.insert("kind".to_string(), serde_json::Value::String(kind.clone()));
                harn_meta.insert("payload".to_string(), payload.clone());
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::HitlResolved {
                session_id,
                request_id,
                kind,
                outcome,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "hitl_resolved",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "requestId".to_string(),
                    serde_json::Value::String(request_id.clone()),
                );
                harn_meta.insert("kind".to_string(), serde_json::Value::String(kind.clone()));
                harn_meta.insert(
                    "outcome".to_string(),
                    serde_json::Value::String(outcome.clone()),
                );
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            // Pipeline-loop milestones — surfaced as ACP `ExtNotification`
            // envelopes under the `_harn/agentEvent` method per the
            // extensibility spec (https://agentclientprotocol.com/protocol/extensibility).
            // ACP defines no `session/update` discriminator for these,
            // and inventing a new one would crash strict client decoders;
            // a `_`-prefixed JSON-RPC method is the spec-blessed path
            // for genuinely new events. Clients that don't know the method
            // SHOULD ignore it (per the spec); clients that do know it
            // discover the contract through `agentCapabilities._meta.harn`.
            AgentEvent::IterationStart {
                session_id,
                iteration,
                provider,
                model,
            } => self.emit_agent_event(
                session_id,
                &payloads::IterationStart {
                    iteration: *iteration,
                    provider: provider.clone(),
                    model: model.clone(),
                },
            ),
            AgentEvent::InferenceSpeed { .. } => {
                self.write_notification(meta::inference_speed_notification(event));
            }
            AgentEvent::ProviderUsage {
                session_id,
                call_id,
                provider,
                model,
                run_usage,
            } => self.emit_agent_event(
                session_id,
                &payloads::ProviderUsage {
                    call_id: call_id.clone(),
                    provider: provider.clone(),
                    model: model.clone(),
                    run_usage: serde_json::to_value(run_usage).expect("provider usage serializes"),
                },
            ),
            AgentEvent::IterationEnd {
                session_id,
                iteration,
                iteration_info,
            } => self.emit_agent_event(
                session_id,
                &payloads::IterationEnd {
                    iteration: *iteration,
                    iteration_info: iteration_info.clone(),
                },
            ),
            AgentEvent::SessionClosed {
                session_id,
                reason,
                status,
                metadata,
            } => self.emit_agent_event(
                session_id,
                &payloads::SessionClosed {
                    reason: reason.clone(),
                    status: status.clone(),
                    metadata: metadata.clone(),
                },
            ),
            AgentEvent::SessionHealth { session_id, fact } => self.emit_agent_event(
                session_id,
                &payloads::SessionHealth::from_value(
                    serde_json::to_value(fact).expect("session health fact serializes"),
                ),
            ),
            AgentEvent::AnchorChanged {
                session_id,
                previous,
                current,
                carry_transcript,
                compacted,
                reason,
            } => self.emit_agent_event(
                session_id,
                &payloads::AnchorChanged {
                    previous: previous.clone(),
                    current: current.clone(),
                    carry_transcript: *carry_transcript,
                    compacted: *compacted,
                    reason: reason.clone(),
                },
            ),
            AgentEvent::TurnPhaseChanged { session_id, phase } => {
                self.emit_agent_event(session_id, &payloads::TurnPhaseChanged::new(phase));
            }
            AgentEvent::JudgeStarted {
                session_id,
                iteration,
                trigger,
            } => self.emit_agent_event(
                session_id,
                &payloads::JudgeStarted {
                    iteration: *iteration,
                    trigger: trigger.clone(),
                },
            ),
            AgentEvent::JudgeDecision {
                session_id,
                iteration,
                verdict,
                reasoning,
                next_step,
                judge_duration_ms,
                source,
                trigger,
                reason,
                confirm,
                converted_from,
                escalation_recommended,
                escalation_target,
                judge_verdict,
                judge_override,
                gap_class,
                judge_outcome,
                terminal_evidence_preserved,
                requirement_report,
            } => self.emit_agent_event(
                session_id,
                &payloads::JudgeDecision {
                    iteration: *iteration,
                    verdict: verdict.clone(),
                    reasoning: reasoning.clone(),
                    next_step: next_step.clone(),
                    judge_duration_ms: *judge_duration_ms,
                    source: source.clone(),
                    trigger: trigger.clone(),
                    reason: reason.clone(),
                    confirm: *confirm,
                    converted_from: converted_from.clone(),
                    escalation_recommended: *escalation_recommended,
                    escalation_target: escalation_target.clone(),
                    judge_verdict: judge_verdict.clone(),
                    judge_override: judge_override.clone(),
                    gap_class: gap_class.clone(),
                    judge_outcome: judge_outcome.clone(),
                    terminal_evidence_preserved: *terminal_evidence_preserved,
                    requirement_report: requirement_report.clone(),
                },
            ),
            AgentEvent::StepJudgeDecision {
                session_id,
                iteration,
                verdict,
                reasoning,
                critique,
                confidence,
                judge_duration_ms,
                vetoed,
                skipped,
                reason,
                judge_error,
                unavailable_reason,
                unavailable_count,
                on_veto,
                input_tokens,
                output_tokens,
                cost_usd,
                provider,
                model,
            } => self.emit_agent_event(
                session_id,
                &payloads::StepJudgeDecision {
                    iteration: *iteration,
                    verdict: verdict.clone(),
                    reasoning: reasoning.clone(),
                    critique: critique.clone(),
                    confidence: *confidence,
                    judge_duration_ms: *judge_duration_ms,
                    vetoed: *vetoed,
                    skipped: *skipped,
                    reason: reason.clone(),
                    judge_error: *judge_error,
                    unavailable_reason: unavailable_reason.clone(),
                    unavailable_count: *unavailable_count,
                    on_veto: on_veto.clone(),
                    input_tokens: *input_tokens,
                    output_tokens: *output_tokens,
                    cost_usd: *cost_usd,
                    provider: provider.clone(),
                    model: model.clone(),
                },
            ),
            AgentEvent::StructuralValidatorDecision {
                session_id,
                iteration,
                rule,
                diagnostic,
                recommended_action,
                vetoed,
                skipped,
                reason,
                on_failure,
                attempts,
                max_attempts,
            } => self.emit_agent_event(
                session_id,
                &payloads::StructuralValidatorDecision {
                    iteration: *iteration,
                    rule: rule.clone(),
                    diagnostic: diagnostic.clone(),
                    recommended_action: recommended_action.clone(),
                    vetoed: *vetoed,
                    skipped: *skipped,
                    reason: reason.clone(),
                    on_failure: on_failure.clone(),
                    attempts: *attempts,
                    max_attempts: *max_attempts,
                },
            ),
            AgentEvent::ScopeClassifierVerdict {
                session_id,
                iteration,
                label,
                original_label,
                confidence,
                confidence_threshold,
                evidence,
                skip_main_turn,
                classifier_kind,
                model,
                error,
            } => self.emit_agent_event(
                session_id,
                &payloads::ScopeClassifierVerdict {
                    iteration: *iteration,
                    label: label.clone(),
                    original_label: original_label.clone(),
                    confidence: *confidence,
                    confidence_threshold: *confidence_threshold,
                    evidence: evidence.clone(),
                    skip_main_turn: *skip_main_turn,
                    classifier_kind: classifier_kind.clone(),
                    model: model.clone(),
                    error: error.clone(),
                },
            ),
            AgentEvent::InputGuardrailVerdict {
                session_id,
                iteration,
                tripwire,
                reason,
                label,
                confidence,
                confidence_threshold,
                classifier_kind,
                model,
                error,
            } => self.emit_agent_event(
                session_id,
                &payloads::InputGuardrailVerdict {
                    iteration: *iteration,
                    tripwire: *tripwire,
                    reason: reason.clone(),
                    label: label.clone(),
                    confidence: *confidence,
                    confidence_threshold: *confidence_threshold,
                    classifier_kind: classifier_kind.clone(),
                    model: model.clone(),
                    error: error.clone(),
                },
            ),
            AgentEvent::MissingToolCallVerdict { session_id, .. } => {
                if let Some(payload) = ext_payloads::missing_tool_call_verdict(event) {
                    self.emit_agent_event(session_id, &payload);
                }
            }
            AgentEvent::RepairOutputContractApplied {
                session_id,
                iteration,
                tool_count,
            } => self.emit_agent_event(
                session_id,
                &payloads::RepairOutputContractApplied {
                    iteration: *iteration,
                    tool_count: *tool_count,
                },
            ),
            AgentEvent::SubagentJoin { .. }
            | AgentEvent::SubagentStop { .. }
            | AgentEvent::RequireSuccessfulToolsViolation { .. }
            | AgentEvent::FinalWrapup { .. }
            | AgentEvent::PackThinkingStripped { .. }
            | AgentEvent::SelfConsistencyTie { .. }
            | AgentEvent::CodeLibrarianQueryNlFallback { .. }
            | AgentEvent::ModelJob { .. } => {
                ext_payloads::emit_documented_stdlib_event(self, event);
            }
            AgentEvent::TypedCheckpoint {
                session_id,
                checkpoint,
            } => self.emit_agent_event(
                session_id,
                &payloads::TypedCheckpoint {
                    checkpoint: checkpoint.clone(),
                },
            ),
            AgentEvent::HostToolResult {
                session_id,
                injection_id,
                tool_call_id,
                tool_name,
                kind,
                raw_input,
                status,
                raw_output,
                result_pointer,
                error,
                duration_ms,
                delivery,
                delivered_at_seam,
                sequence,
                provenance,
                sanitization,
            } => self.emit_agent_event(
                session_id,
                &payloads::HostToolResult {
                    injection_id: injection_id.clone(),
                    tool_call_id: tool_call_id.clone(),
                    tool_name: tool_name.clone(),
                    tool_kind: kind.as_ref().map(to_wire),
                    raw_input: raw_input.clone(),
                    status: Self::status_str(*status).to_string(),
                    raw_output: raw_output.clone(),
                    result_pointer: result_pointer.clone(),
                    error: error.clone(),
                    duration_ms: *duration_ms,
                    delivery: to_wire(delivery),
                    delivered_at_seam: delivered_at_seam.clone(),
                    sequence: *sequence,
                    provenance: to_wire(provenance),
                    sanitization: to_wire(sanitization),
                },
            ),
            AgentEvent::HostAttachment {
                session_id,
                injection_id,
                media_type,
                flavor,
                artifact_pointer,
                sha256,
                size_bytes,
                rendered,
                description,
                description_model,
                delivery,
                delivered_at_seam,
                sequence,
                provenance,
                sanitization,
            } => self.emit_agent_event(
                session_id,
                &payloads::HostAttachment {
                    injection_id: injection_id.clone(),
                    media_type: media_type.clone(),
                    flavor: to_wire(flavor),
                    artifact_pointer: artifact_pointer.clone(),
                    sha256: sha256.clone(),
                    size_bytes: *size_bytes,
                    rendered: to_wire(rendered),
                    description: description.clone(),
                    description_model: description_model.clone(),
                    delivery: to_wire(delivery),
                    delivered_at_seam: delivered_at_seam.clone(),
                    sequence: *sequence,
                    provenance: to_wire(provenance),
                    sanitization: to_wire(sanitization),
                },
            ),
            AgentEvent::Artifact {
                session_id,
                artifact_id,
                kind,
                title,
                mime_type,
                spec,
                fallback,
                size_bytes,
                provenance,
                metadata,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "artifact",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "artifactId".to_string(),
                    serde_json::Value::String(artifact_id.clone()),
                );
                harn_meta.insert("kind".to_string(), serde_json::Value::String(kind.clone()));
                harn_meta.insert(
                    "title".to_string(),
                    title
                        .as_ref()
                        .map(|value| serde_json::Value::String(value.clone()))
                        .unwrap_or(serde_json::Value::Null),
                );
                harn_meta.insert(
                    "mimeType".to_string(),
                    serde_json::Value::String(mime_type.clone()),
                );
                harn_meta.insert("spec".to_string(), spec.clone());
                harn_meta.insert(
                    "fallback".to_string(),
                    serde_json::Value::String(fallback.clone()),
                );
                harn_meta.insert(
                    "sizeBytes".to_string(),
                    serde_json::Value::from(*size_bytes),
                );
                harn_meta.insert("provenance".to_string(), provenance.clone());
                harn_meta.insert("metadata".to_string(), metadata.clone());
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            event @ (AgentEvent::PurposeLabel { .. }
            | AgentEvent::ProgressReported { .. }
            | AgentEvent::CompassRoutingDecision { .. }
            | AgentEvent::AgentScratchpadReorganization { .. }
            | AgentEvent::FeedbackInjected { .. }
            | AgentEvent::BudgetExhausted { .. }
            | AgentEvent::BudgetCircuitBreaker { .. }
            | AgentEvent::LoopStuck { .. }
            | AgentEvent::LoopStuckSignal { .. }
            | AgentEvent::ReservedTerminalVerify { .. }
            | AgentEvent::DaemonWatchdogTripped { .. }
            | AgentEvent::LoopControlDecision { .. }
            | AgentEvent::AgentLoopStallWarning { .. }
            | AgentEvent::CapabilityGap { .. }
            | AgentEvent::BoundaryFailure { .. }
            | AgentEvent::ToolFormatOverride { .. }
            | AgentEvent::ToolCallAudit { .. }
            | AgentEvent::ToolBatchDisposition { .. }
            | AgentEvent::CompositionStart { .. }
            | AgentEvent::CompositionChildCall { .. }
            | AgentEvent::CompositionChildResult { .. }
            | AgentEvent::CompositionFinish { .. }
            | AgentEvent::CompositionError { .. }
            | AgentEvent::LoopCheckpoint { .. }
            | AgentEvent::McpNotification { .. }
            | AgentEvent::McpCatalogChanged { .. }
            | AgentEvent::McpAuthRequired { .. }
            | AgentEvent::OrchestrationDecision { .. }) => {
                orchestration_events::handle(self, event);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod boundary_tests;
mod ext_payloads;
mod meta;
mod orchestration_events;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
mod tool_lifecycle;

pub(super) use meta::merge_harn_meta;
