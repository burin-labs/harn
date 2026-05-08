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
    replayed: bool,
}

impl AcpAgentEventSink {
    pub(super) fn new(output: AcpOutput) -> Self {
        Self {
            output,
            replayed: false,
        }
    }

    pub(super) fn for_replay(output: AcpOutput) -> Self {
        Self {
            output,
            replayed: true,
        }
    }

    fn write_notification(&self, params: serde_json::Value) {
        self.write_jsonrpc_notification("session/update", params);
    }

    /// Emit an arbitrary JSON-RPC notification through the same transport
    /// as `session/update`. Used for ACP `ExtNotification` envelopes
    /// (methods prefixed with `_`, per the ACP extensibility spec) when
    /// the canonical `session/update` discriminator has no slot for the
    /// event being surfaced — currently the pipeline-loop milestones
    /// emitted via `_harn/agentEvent`.
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

    /// Emit a pipeline-loop milestone as an ACP `ExtNotification` under
    /// the `_harn/agentEvent` method. The schema is the per-`kind`
    /// payload defined in `HARN_AGENT_EVENT_KINDS` — `sessionId` + `kind`
    /// are required at the top level, and kind-specific fields ride
    /// directly under `params` (no nested `_meta` wrapper, since the
    /// whole notification is already vendor-prefixed).
    fn emit_agent_event_ext(&self, kind: &str, session_id: &str, mut payload: serde_json::Value) {
        let obj = payload
            .as_object_mut()
            .expect("emit_agent_event_ext: payload must be a JSON object");
        obj.insert(
            "sessionId".to_string(),
            serde_json::Value::String(session_id.to_string()),
        );
        obj.insert(
            "kind".to_string(),
            serde_json::Value::String(kind.to_string()),
        );
        self.write_jsonrpc_notification(super::schema::HARN_AGENT_EVENT_METHOD, payload);
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

/// Merge `harn_meta` keys into `value._meta.harn`, creating intermediate
/// objects as needed. Existing `_meta.harn` keys are preserved (unless
/// overwritten by `harn_meta`). No-op when `harn_meta` is empty or
/// `value` is not a JSON object.
pub(super) fn merge_harn_meta(
    value: &mut serde_json::Value,
    harn_meta: serde_json::Map<String, serde_json::Value>,
) {
    if harn_meta.is_empty() {
        return;
    }
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    let meta = obj
        .entry("_meta".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(meta_obj) = meta.as_object_mut() else {
        return;
    };
    let harn = meta_obj
        .entry("harn".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(harn_obj) = harn.as_object_mut() else {
        return;
    };
    for (k, v) in harn_meta {
        harn_obj.insert(k, v);
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
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": content_block,
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
                let mut harn_meta = serde_json::Map::new();
                if let Some(p) = parsing {
                    harn_meta.insert("parsing".to_string(), serde_json::Value::Bool(*p));
                }
                if let Some(record) = audit {
                    if let Ok(value) = serde_json::to_value(record) {
                        harn_meta.insert("audit".to_string(), value);
                    }
                }
                Self::attach_harn_meta(&mut update, harn_meta);
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
                let mut harn_meta = serde_json::Map::new();
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
                if let Some(exec) = executor {
                    harn_meta.insert("executor".to_string(), Self::executor_to_json(exec));
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
                Self::attach_harn_meta(&mut update, harn_meta);
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
                mode,
                strategy,
                archived_messages,
                estimated_tokens_before,
                estimated_tokens_after,
                snapshot_asset_id,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "transcript_compacted",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert("mode".to_string(), serde_json::Value::String(mode.clone()));
                harn_meta.insert(
                    "strategy".to_string(),
                    serde_json::Value::String(strategy.clone()),
                );
                harn_meta.insert(
                    "archivedMessages".to_string(),
                    serde_json::Value::from(*archived_messages),
                );
                harn_meta.insert(
                    "estimatedTokensBefore".to_string(),
                    serde_json::Value::from(*estimated_tokens_before),
                );
                harn_meta.insert(
                    "estimatedTokensAfter".to_string(),
                    serde_json::Value::from(*estimated_tokens_after),
                );
                harn_meta.insert(
                    "snapshotAssetId".to_string(),
                    match snapshot_asset_id {
                        Some(id) => serde_json::Value::String(id.clone()),
                        None => serde_json::Value::Null,
                    },
                );
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
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
            AgentEvent::TurnStart {
                session_id,
                iteration,
            } => {
                self.emit_agent_event_ext(
                    "turn_start",
                    session_id,
                    serde_json::json!({"iteration": iteration}),
                );
            }
            AgentEvent::TurnEnd {
                session_id,
                iteration,
                turn_info,
            } => {
                self.emit_agent_event_ext(
                    "turn_end",
                    session_id,
                    serde_json::json!({"iteration": iteration, "turnInfo": turn_info}),
                );
            }
            AgentEvent::JudgeDecision {
                session_id,
                iteration,
                verdict,
                reasoning,
                next_step,
                judge_duration_ms,
            } => {
                self.emit_agent_event_ext(
                    "judge_decision",
                    session_id,
                    serde_json::json!({
                        "iteration": iteration,
                        "verdict": verdict,
                        "reasoning": reasoning,
                        "nextStep": next_step,
                        "judgeDurationMs": judge_duration_ms,
                    }),
                );
            }
            AgentEvent::TypedCheckpoint {
                session_id,
                checkpoint,
            } => {
                self.emit_agent_event_ext(
                    "typed_checkpoint",
                    session_id,
                    serde_json::json!({"checkpoint": checkpoint}),
                );
            }
            AgentEvent::FeedbackInjected {
                session_id,
                kind,
                content,
            } => {
                self.emit_agent_event_ext(
                    "feedback_injected",
                    session_id,
                    serde_json::json!({"feedbackKind": kind, "content": content}),
                );
            }
            AgentEvent::BudgetExhausted {
                session_id,
                max_iterations,
            } => {
                self.emit_agent_event_ext(
                    "budget_exhausted",
                    session_id,
                    serde_json::json!({"maxIterations": max_iterations}),
                );
            }
            AgentEvent::LoopStuck {
                session_id,
                max_nudges,
                last_iteration,
                tail_excerpt,
            } => {
                self.emit_agent_event_ext(
                    "loop_stuck",
                    session_id,
                    serde_json::json!({
                        "maxNudges": max_nudges,
                        "lastIteration": last_iteration,
                        "tailExcerpt": tail_excerpt,
                    }),
                );
            }
            AgentEvent::DaemonWatchdogTripped {
                session_id,
                attempts,
                elapsed_ms,
            } => {
                self.emit_agent_event_ext(
                    "daemon_watchdog_tripped",
                    session_id,
                    serde_json::json!({"attempts": attempts, "elapsedMs": elapsed_ms}),
                );
            }
            AgentEvent::LoopControlDecision {
                session_id,
                iteration,
                action,
                old_limit,
                new_limit,
                reason,
                status,
            } => {
                self.emit_agent_event_ext(
                    "loop_control_decision",
                    session_id,
                    serde_json::json!({
                        "iteration": iteration,
                        "action": action,
                        "oldLimit": old_limit,
                        "newLimit": new_limit,
                        "reason": reason,
                        "status": status,
                    }),
                );
            }
            AgentEvent::ToolCallAudit {
                session_id,
                tool_call_id,
                tool_name,
                audit,
            } => {
                self.emit_agent_event_ext(
                    "tool_call_audit",
                    session_id,
                    serde_json::json!({
                        "toolCallId": tool_call_id,
                        "toolName": tool_name,
                        "audit": audit,
                    }),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
