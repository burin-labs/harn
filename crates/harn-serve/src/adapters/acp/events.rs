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
        self.write_jsonrpc_notification(super::HARN_AGENT_EVENT_METHOD, payload);
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
            // SHOULD ignore it (per the spec); burin-code subscribes via
            // the `extensionMethods` advertisement in `agentCapabilities._meta.harn`.
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

    use super::super::{
        HARN_AGENT_EVENT_KINDS, HARN_AGENT_EVENT_METHOD, HARN_SESSION_UPDATE_EXTENSIONS,
    };
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

    fn update_harn_meta(payload: &serde_json::Value) -> &serde_json::Value {
        &payload["params"]["update"]["_meta"]["harn"]
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
                uri: None,
            },
            task: "Review the patch".to_string(),
            reason: "Merge queue requires review".to_string(),
            created_at: "2026-04-28T00:00:00Z".to_string(),
            ..Default::default()
        }
        .normalize()
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
            AgentEvent::HitlRequested {
                session_id: "session-1".into(),
                request_id: "hitl_question_session-1_1".into(),
                kind: "question".into(),
                payload: serde_json::json!({"prompt": "Approve deploy?"}),
            },
            AgentEvent::HitlResolved {
                session_id: "session-1".into(),
                request_id: "hitl_question_session-1_1".into(),
                kind: "question".into(),
                outcome: "answered".into(),
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

    fn agent_event_ext_fixture_events() -> Vec<AgentEvent> {
        vec![
            AgentEvent::TurnStart {
                session_id: "session-1".to_string(),
                iteration: 0,
            },
            AgentEvent::TurnEnd {
                session_id: "session-1".to_string(),
                iteration: 0,
                turn_info: serde_json::json!({
                    "tool_calls": 2,
                    "tool_names": ["read_file", "grep"]
                }),
            },
            AgentEvent::JudgeDecision {
                session_id: "session-1".to_string(),
                iteration: 0,
                verdict: "continue".to_string(),
                reasoning: "needs one more concrete action".to_string(),
                next_step: Some("run the verifier".to_string()),
                judge_duration_ms: 42,
            },
            AgentEvent::FeedbackInjected {
                session_id: "session-1".to_string(),
                kind: "protocol_violation".to_string(),
                content: "missed required tool call; reissuing".to_string(),
            },
            AgentEvent::BudgetExhausted {
                session_id: "session-1".to_string(),
                max_iterations: 8,
            },
            AgentEvent::LoopStuck {
                session_id: "session-1".to_string(),
                max_nudges: 3,
                last_iteration: 4,
                tail_excerpt: "still thinking...".to_string(),
            },
            AgentEvent::DaemonWatchdogTripped {
                session_id: "session-1".to_string(),
                attempts: 5,
                elapsed_ms: 12_000,
            },
        ]
    }

    /// Pipeline-loop milestone events ride on the ACP `ExtNotification`
    /// channel via `_harn/agentEvent`. The fixture pins the wire shape
    /// per kind so any drift in field names (e.g. snake_case vs.
    /// camelCase) or payload structure trips a build-time failure
    /// rather than silently breaking burin-code's decoder. Every kind
    /// in the fixture must also appear in `HARN_AGENT_EVENT_KINDS` so
    /// the capability advertisement stays honest.
    #[tokio::test(flavor = "current_thread")]
    async fn agent_event_ext_notification_fixtures_are_pinned() {
        let actual = collect_notifications(agent_event_ext_fixture_events()).await;
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/acp/agent_event_ext_notifications.json"
        ))
        .expect("fixture json");
        assert_eq!(serde_json::Value::Array(actual.clone()), expected);

        for notification in actual {
            assert_eq!(
                notification["method"].as_str().expect("method"),
                HARN_AGENT_EVENT_METHOD,
                "every pipeline-loop milestone notification must use the \
                 advertised _harn/agentEvent method"
            );
            assert!(
                notification["params"]["sessionId"].is_string(),
                "sessionId must be a top-level string on every agent event"
            );
            let kind = notification["params"]["kind"]
                .as_str()
                .expect("kind discriminator");
            assert!(
                HARN_AGENT_EVENT_KINDS.contains(&kind),
                "{kind} is not advertised in HARN_AGENT_EVENT_KINDS — clients \
                 cannot subscribe to undocumented kinds"
            );
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
            // Vendor-extension fields ride under `_meta.harn` per harn#905.
            let harn_meta = update_harn_meta(&payload);
            assert_eq!(harn_meta["workerId"], "worker-1");
            assert_eq!(harn_meta["workerName"], "review");
            assert_eq!(harn_meta["workerTask"], "review pr");
            assert_eq!(harn_meta["workerMode"], "delegated_stage");
            assert_eq!(harn_meta["event"], worker_event.as_str());
            assert_eq!(harn_meta["status"], status);
            assert_eq!(harn_meta["terminal"], terminal);
            assert_eq!(harn_meta["metadata"]["child_run_id"], "run_x");
            assert_eq!(harn_meta["audit"]["run_id"], "run_x");
            assert!(update.get("workerId").is_none());
            assert!(update.get("audit").is_none());
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
        let harn_meta = update_harn_meta(&payload);
        assert!(harn_meta.get("audit").is_none());
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
                        uri: None,
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
        // Vendor-extension fields ride under `_meta.harn` per harn#905.
        let harn_meta = update_harn_meta(&payload);
        assert_eq!(harn_meta["handoffId"], "handoff-1");
        assert_eq!(
            harn_meta["handoff"]["target_persona_or_human"]["label"],
            "review_captain"
        );
        assert!(payload["params"]["update"].get("handoffId").is_none());
        assert!(payload["params"]["update"].get("handoff").is_none());
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
                uri: None,
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
        let harn_meta = update_harn_meta(&payload);
        assert_eq!(harn_meta["errorCategory"], "schema_validation");
        assert_eq!(harn_meta["error"], "missing required arg `path`");
        assert!(payload["params"]["update"].get("errorCategory").is_none());
        assert!(payload["params"]["update"].get("error").is_none());
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
        assert!(payload["params"]["update"].get("_meta").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_carries_parsing_flag_through_to_acp_wire() {
        // Harn#692/#904: candidate parser state is Harn metadata on
        // the ACP wire so clients can render the in-flight chip without
        // extending the root ACP tool-call shape.
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
        assert_eq!(update_harn_meta(&payload)["parsing"], true);
        assert!(payload["params"]["update"].get("parsing").is_none());

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
        let harn_meta = update_harn_meta(&payload);
        assert_eq!(harn_meta["parsing"], false);
        assert_eq!(harn_meta["errorCategory"], "parse_aborted");
        assert!(payload["params"]["update"].get("parsing").is_none());
        assert!(payload["params"]["update"].get("errorCategory").is_none());

        // Default `parsing: None` must not surface Harn metadata at all.
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
        assert!(payload["params"]["update"].get("_meta").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_update_serializes_executor_per_acp_wire_format() {
        // Harn#691/#904: clients render badges off Harn executor metadata.
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
            assert_eq!(update_harn_meta(&payload)["executor"], expected);
            assert!(payload["params"]["update"].get("executor").is_none());
        }

        // `executor: None` must not surface Harn metadata.
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
        assert!(payload["params"]["update"].get("_meta").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn tool_call_update_streams_raw_input_and_raw_input_partial_per_acp_wire_format() {
        // #693/#904: parsed raw input remains canonical `rawInput`;
        // unparseable raw bytes are Harn metadata under `_meta.harn`.
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
        assert!(payload["params"]["update"].get("_meta").is_none());

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
            update_harn_meta(&payload)["rawInputPartial"],
            r#"{"q":"hel"#
        );
        assert!(payload["params"]["update"].get("rawInputPartial").is_none());

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
        assert!(update_harn_meta(&payload).get("rawInputPartial").is_none());
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
        let audit_value = &update_harn_meta(&payload)["audit"];
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
        assert!(payload["params"]["update"].get("audit").is_none());
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
            payload["params"]["update"].get("_meta").is_none(),
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
        let harn_meta = update_harn_meta(&payload);
        assert_eq!(harn_meta["audit"]["session_id"], "session_42");
        assert_eq!(harn_meta["audit"]["run_id"], "run_42");
        assert_eq!(harn_meta["audit"]["mutation_scope"], "apply_workspace");
        assert_eq!(harn_meta["audit"]["execution_kind"], "workflow");
        assert_eq!(harn_meta["executor"], "host_bridge");
        assert_eq!(harn_meta["durationMs"], 11);
        assert_eq!(harn_meta["executionDurationMs"], 7);
        assert!(update.get("audit").is_none());
        assert!(update.get("executor").is_none());
        assert!(update.get("durationMs").is_none());
        assert!(update.get("executionDurationMs").is_none());
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
            payload["params"]["update"].get("_meta").is_none(),
            "got: {payload}"
        );
    }

    /// harn#905 conformance: vendor-extension session-update fields
    /// must travel under `update._meta.harn` and **must not** appear at
    /// the update root. Canonical ACP fields (`sessionUpdate`, `content`,
    /// etc.) stay at their canonical locations. This test pins the
    /// contract field-by-field for every `HARN_SESSION_UPDATE_EXTENSIONS`
    /// variant the adapter emits so a regression in any one variant
    /// fails this single test.
    #[tokio::test(flavor = "current_thread")]
    async fn vendor_extension_session_update_fields_live_under_meta_harn() {
        let actual = collect_notifications(extension_fixture_events()).await;

        let expectations: &[(&str, &[&str])] = &[
            ("skill_activated", &["skillName", "iteration", "reason"]),
            ("skill_deactivated", &["skillName", "iteration"]),
            ("skill_scope_tools", &["skillName", "allowedTools"]),
            (
                "tool_search_query",
                &["toolUseId", "name", "query", "strategy", "mode"],
            ),
            (
                "tool_search_result",
                &["toolUseId", "promoted", "strategy", "mode"],
            ),
            (
                "transcript_compacted",
                &[
                    "mode",
                    "strategy",
                    "archivedMessages",
                    "estimatedTokensBefore",
                    "estimatedTokensAfter",
                    "snapshotAssetId",
                ],
            ),
            ("handoff", &["handoffId", "artifactId", "handoff"]),
            ("fs_watch", &["subscriptionId", "events"]),
            (
                "worker_update",
                &[
                    "workerId",
                    "workerName",
                    "workerTask",
                    "workerMode",
                    "event",
                    "status",
                    "terminal",
                    "metadata",
                    "audit",
                ],
            ),
            ("hitl_request", &["requestId", "kind", "payload"]),
            ("hitl_resolved", &["requestId", "kind", "outcome"]),
        ];

        assert_eq!(
            actual.len(),
            expectations.len(),
            "fixture event count must match expectations table"
        );

        for (notification, (variant, vendor_fields)) in actual.iter().zip(expectations.iter()) {
            let update = &notification["params"]["update"];
            assert_eq!(
                update["sessionUpdate"], *variant,
                "update[{variant}] must keep canonical sessionUpdate at the root"
            );
            let harn_meta = &update["_meta"]["harn"];
            assert!(
                harn_meta.is_object(),
                "update[{variant}] must carry _meta.harn object, got: {update}"
            );
            for field in *vendor_fields {
                assert!(
                    harn_meta.get(field).is_some(),
                    "update[{variant}]._meta.harn must contain `{field}`, got: {harn_meta}"
                );
                assert!(
                    update.get(field).is_none(),
                    "update[{variant}].`{field}` must not be emitted at the root, got: {update}"
                );
            }
            // No vendor field other than `_meta` and `sessionUpdate`
            // should be present at the update root.
            let update_obj = update.as_object().expect("update is object");
            for key in update_obj.keys() {
                assert!(
                    matches!(key.as_str(), "sessionUpdate" | "_meta"),
                    "update[{variant}] must not carry root key `{key}` (vendor extension); got: {update}"
                );
            }
        }
    }

    /// harn#905 conformance: `progress` and `log` are emitted by
    /// `AcpBridge` (not `AcpAgentEventSink`), so cover them with a
    /// dedicated bridge-side test. Both variants are entirely
    /// vendor — every field other than `sessionUpdate` itself must
    /// land under `_meta.harn`.
    #[tokio::test(flavor = "current_thread")]
    async fn bridge_progress_and_log_session_updates_namespace_vendor_fields() {
        use std::collections::HashMap;
        use std::rc::Rc;
        use std::sync::atomic::AtomicU64;
        use std::sync::Arc;
        use tokio::sync::Mutex as TokioMutex;

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, mut rx) = mpsc::unbounded_channel();
                let bridge = Rc::new(super::super::AcpBridge {
                    session_id: "session-1".to_string(),
                    output: AcpOutput::Channel(tx),
                    pending: Arc::new(TokioMutex::new(HashMap::new())),
                    next_id_counter: AtomicU64::new(1),
                    cancellation: super::super::SessionCancellation::default(),
                    script_name: std::sync::Mutex::new(String::new()),
                    assistant_state: std::sync::Mutex::new(
                        harn_vm::visible_text::VisibleTextState::default(),
                    ),
                });

                bridge.send_progress(
                    "ingest",
                    "loading",
                    Some(3),
                    Some(10),
                    Some(serde_json::json!({"item": "row-7"})),
                );
                let line = rx.recv().await.expect("progress notification");
                let payload: serde_json::Value =
                    serde_json::from_str(&line).expect("progress json");
                let update = &payload["params"]["update"];
                assert_eq!(update["sessionUpdate"], "progress");
                let harn_meta = &update["_meta"]["harn"];
                assert_eq!(harn_meta["phase"], "ingest");
                assert_eq!(harn_meta["message"], "loading");
                assert_eq!(harn_meta["progress"], 3);
                assert_eq!(harn_meta["total"], 10);
                assert_eq!(harn_meta["data"]["item"], "row-7");
                for forbidden in ["phase", "message", "progress", "total", "data"] {
                    assert!(
                        update.get(forbidden).is_none(),
                        "progress.{forbidden} must live under _meta.harn, got: {update}"
                    );
                }

                bridge.send_log(
                    "warn",
                    "deprecated builtin: foo",
                    Some(serde_json::json!({"builtin": "foo"})),
                );
                let line = rx.recv().await.expect("log notification");
                let payload: serde_json::Value = serde_json::from_str(&line).expect("log json");
                let update = &payload["params"]["update"];
                assert_eq!(update["sessionUpdate"], "log");
                let harn_meta = &update["_meta"]["harn"];
                assert_eq!(harn_meta["level"], "warn");
                assert_eq!(harn_meta["message"], "deprecated builtin: foo");
                assert_eq!(harn_meta["fields"]["builtin"], "foo");
                for forbidden in ["level", "message", "fields"] {
                    assert!(
                        update.get(forbidden).is_none(),
                        "log.{forbidden} must live under _meta.harn, got: {update}"
                    );
                }

                // Optional fields are simply absent under `_meta.harn`,
                // not promoted back to the root.
                bridge.send_progress("ingest", "starting", None, None, None);
                let line = rx.recv().await.expect("minimal progress notification");
                let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
                let update = &payload["params"]["update"];
                let harn_meta = &update["_meta"]["harn"];
                assert!(harn_meta.get("progress").is_none());
                assert!(harn_meta.get("total").is_none());
                assert!(harn_meta.get("data").is_none());
                assert!(update.get("progress").is_none());
            })
            .await;
    }

    /// harn#905 conformance: `agent_message_chunk` is canonical, so the
    /// content block and its `text` field stay at the canonical
    /// location; only the harn-specific `visible_text` /
    /// `visible_delta` content extensions move under `content._meta.harn`.
    #[tokio::test(flavor = "current_thread")]
    async fn agent_message_chunk_visible_text_lives_under_content_meta_harn() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
        sink.handle_event(&AgentEvent::AgentMessageChunk {
            session_id: "session-1".to_string(),
            content: "hello".to_string(),
        });
        let line = rx.recv().await.expect("agent_message_chunk notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        let content = &payload["params"]["update"]["content"];
        assert_eq!(content["type"], "text");
        assert_eq!(content["text"], "hello");
        assert_eq!(content["_meta"]["harn"]["visible_text"], "hello");
        assert_eq!(content["_meta"]["harn"]["visible_delta"], "hello");
        assert!(content.get("visible_text").is_none());
        assert!(content.get("visible_delta").is_none());
    }

    /// Pipeline-loop milestones used to be silently dropped by the ACP
    /// adapter (no canonical `session/update` slot). They now ride on
    /// the `_harn/agentEvent` `ExtNotification` channel — never on
    /// `session/update`. This test pins the negative half of that
    /// contract: even though the events are surfaced, they MUST NOT
    /// pollute the canonical `session/update` stream that strict ACP
    /// clients consume by closed enum.
    #[test]
    fn internal_agent_events_never_emit_session_updates() {
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

        let mut count = 0;
        while let Ok(line) = rx.try_recv() {
            count += 1;
            let payload: serde_json::Value =
                serde_json::from_str(&line).expect("notification json");
            assert_ne!(
                payload["method"], "session/update",
                "pipeline-loop milestones must NOT ride on session/update — \
                 strict ACP clients use a closed enum and would reject any \
                 vendor-invented sessionUpdate kind. Got: {payload}"
            );
            assert_eq!(
                payload["method"], HARN_AGENT_EVENT_METHOD,
                "pipeline-loop milestones MUST ride on the advertised \
                 _harn/agentEvent ExtNotification method"
            );
        }
        assert_eq!(
            count, 6,
            "expected one ExtNotification per fed AgentEvent, got {count}"
        );
    }
}
