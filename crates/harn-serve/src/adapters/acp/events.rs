//! ACP AgentEventSink — translates canonical `AgentEvent` variants into ACP
//! `session/update` notifications. Registered per-session at prompt start.

use harn_vm::agent_events::{AgentEvent, AgentEventSink, ToolExecutor};
use harn_vm::composition::{CompositionChildCall, CompositionChildResult, CompositionRunEnvelope};
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

    fn composition_run_to_json(run: &CompositionRunEnvelope) -> serde_json::Value {
        serde_json::json!({
            "runId": &run.run_id,
            "language": &run.language,
            "snippetHash": &run.snippet_hash,
            "bindingManifestHash": &run.binding_manifest_hash,
            "requestedSideEffectCeiling": run.requested_side_effect_ceiling.as_str(),
            "stdout": &run.stdout,
            "stderr": &run.stderr,
            "artifacts": &run.artifacts,
            "result": &run.result,
            "failureCategory": run.failure_category.map(|category| category.as_str()),
            "error": &run.error,
            "durationMs": run.duration_ms,
            "metadata": &run.metadata,
        })
    }

    fn composition_child_call_to_json(call: &CompositionChildCall) -> serde_json::Value {
        serde_json::json!({
            "runId": &call.run_id,
            "toolCallId": &call.tool_call_id,
            "toolName": &call.tool_name,
            "operationIndex": call.operation_index,
            "annotations": &call.annotations,
            "requestedSideEffectLevel": call.requested_side_effect_level.as_str(),
            "policyContext": &call.policy_context,
            "rawInput": &call.raw_input,
        })
    }

    fn composition_child_result_to_json(result: &CompositionChildResult) -> serde_json::Value {
        serde_json::json!({
            "runId": &result.run_id,
            "toolCallId": &result.tool_call_id,
            "toolName": &result.tool_name,
            "operationIndex": result.operation_index,
            "status": Self::status_str(result.status),
            "rawOutput": &result.raw_output,
            "error": &result.error,
            "errorCategory": result.error_category.map(|category| category.as_str()),
            "executor": result.executor.as_ref().map(Self::executor_to_json),
            "durationMs": result.duration_ms,
            "executionDurationMs": result.execution_duration_ms,
        })
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

fn has_progress_entries(entries: &serde_json::Value) -> bool {
    entries
        .as_array()
        .map(|entries| !entries.is_empty())
        .unwrap_or(false)
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
            AgentEvent::UserMessage {
                session_id,
                message_id,
                content,
            } => {
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": {
                        "sessionUpdate": "user_message",
                        "messageId": message_id,
                        "content": content,
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
            AgentEvent::SkillNarrow {
                session_id,
                reason,
                removed_tools,
                remaining_tools,
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
                reason,
                strategy,
                archived_messages,
                estimated_tokens_before,
                estimated_tokens_after,
                snapshot_asset_id,
                instruction_mode,
                instruction_source,
                compaction_policy,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "transcript_compacted",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert("mode".to_string(), serde_json::Value::String(mode.clone()));
                harn_meta.insert(
                    "reason".to_string(),
                    serde_json::Value::String(reason.clone()),
                );
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
                if let Some(instruction_mode) = instruction_mode {
                    harn_meta.insert(
                        "instructionMode".to_string(),
                        serde_json::Value::String(instruction_mode.clone()),
                    );
                }
                if let Some(instruction_source) = instruction_source {
                    harn_meta.insert(
                        "instructionSource".to_string(),
                        serde_json::Value::String(instruction_source.clone()),
                    );
                }
                if let Some(compaction_policy) = compaction_policy {
                    harn_meta.insert("compactionPolicy".to_string(), compaction_policy.clone());
                }
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
                merge_harn_meta(&mut update, harn_meta);
                self.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": update,
                }));
            }
            AgentEvent::ReminderEmitted {
                session_id,
                reminder_id,
                tags,
                body,
                role_hint,
                rendered_role,
                source,
                ttl_turns,
            } => {
                let mut update = serde_json::json!({
                    "sessionUpdate": "reminder_emitted",
                });
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "reminder".to_string(),
                    serde_json::json!({
                        "reminderId": reminder_id,
                        "tags": tags,
                        "body": body,
                        "roleHint": role_hint,
                        "renderedRole": rendered_role,
                        "source": source,
                        "ttlTurns": ttl_turns,
                    }),
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
            AgentEvent::StagedWritesPending {
                session_id,
                pending_count,
                total_bytes,
            } => {
                let mut update = super::bridge::progress_update(
                    "fs_staging",
                    "staged writes pending",
                    Some(*pending_count as i64),
                    None,
                    None,
                );
                let mut harn_meta = serde_json::Map::new();
                harn_meta.insert(
                    "kind".to_string(),
                    serde_json::Value::String("staged_writes_pending".to_string()),
                );
                harn_meta.insert(
                    "pendingCount".to_string(),
                    serde_json::Value::from(*pending_count as u64),
                );
                harn_meta.insert(
                    "totalBytes".to_string(),
                    serde_json::Value::from(*total_bytes),
                );
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
                let mut payload = serde_json::json!({
                    "controlId": control_id,
                    "method": method,
                    "outcome": outcome,
                    "status": status,
                    "actor": actor,
                    "target": target,
                });
                if let Some(reason) = reason {
                    payload["reason"] = serde_json::Value::String(reason.clone());
                }
                if !metadata.is_null() {
                    payload["metadata"] = metadata.clone();
                }
                self.emit_agent_event_ext("control_outcome", session_id, payload);
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
            AgentEvent::IterationStart {
                session_id,
                iteration,
                provider,
                model,
            } => {
                let mut payload = serde_json::json!({"iteration": iteration});
                if !provider.is_empty() {
                    payload["provider"] = serde_json::Value::String(provider.clone());
                }
                if !model.is_empty() {
                    payload["model"] = serde_json::Value::String(model.clone());
                }
                self.emit_agent_event_ext("iteration_start", session_id, payload);
            }
            AgentEvent::IterationEnd {
                session_id,
                iteration,
                iteration_info,
            } => {
                self.emit_agent_event_ext(
                    "iteration_end",
                    session_id,
                    serde_json::json!({"iteration": iteration, "iterationInfo": iteration_info}),
                );
            }
            AgentEvent::SessionClosed {
                session_id,
                reason,
                status,
                metadata,
            } => {
                self.emit_agent_event_ext(
                    "session_closed",
                    session_id,
                    serde_json::json!({
                        "reason": reason,
                        "status": status,
                        "metadata": metadata,
                    }),
                );
            }
            AgentEvent::AnchorChanged {
                session_id,
                previous,
                current,
                carry_transcript,
                compacted,
                reason,
            } => {
                self.emit_agent_event_ext(
                    "anchor_changed",
                    session_id,
                    serde_json::json!({
                        "previous": previous,
                        "current": current,
                        "carryTranscript": carry_transcript,
                        "compacted": compacted,
                        "reason": reason,
                    }),
                );
            }
            AgentEvent::JudgeDecision {
                session_id,
                iteration,
                verdict,
                reasoning,
                next_step,
                judge_duration_ms,
                trigger,
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
                        "trigger": trigger,
                    }),
                );
            }
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
                on_veto,
                input_tokens,
                output_tokens,
                cost_usd,
                provider,
                model,
            } => {
                self.emit_agent_event_ext(
                    "step_judge_decision",
                    session_id,
                    serde_json::json!({
                        "iteration": iteration,
                        "verdict": verdict,
                        "reasoning": reasoning,
                        "critique": critique,
                        "confidence": confidence,
                        "judgeDurationMs": judge_duration_ms,
                        "vetoed": vetoed,
                        "skipped": skipped,
                        "reason": reason,
                        "onVeto": on_veto,
                        "inputTokens": input_tokens,
                        "outputTokens": output_tokens,
                        "costUsd": cost_usd,
                        "provider": provider,
                        "model": model,
                    }),
                );
            }
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
            } => {
                self.emit_agent_event_ext(
                    "structural_validator_decision",
                    session_id,
                    serde_json::json!({
                        "iteration": iteration,
                        "rule": rule,
                        "diagnostic": diagnostic,
                        "recommendedAction": recommended_action,
                        "vetoed": vetoed,
                        "skipped": skipped,
                        "reason": reason,
                        "onFailure": on_failure,
                        "attempts": attempts,
                        "maxAttempts": max_attempts,
                    }),
                );
            }
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
            } => {
                self.emit_agent_event_ext(
                    "scope_classifier_verdict",
                    session_id,
                    serde_json::json!({
                        "iteration": iteration,
                        "label": label,
                        "originalLabel": original_label,
                        "confidence": confidence,
                        "confidenceThreshold": confidence_threshold,
                        "evidence": evidence,
                        "skipMainTurn": skip_main_turn,
                        "classifierKind": classifier_kind,
                        "model": model,
                        "error": error,
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
            AgentEvent::ProgressReported {
                session_id,
                message,
                entries,
                replace,
                metadata,
            } => {
                if has_progress_entries(entries) {
                    self.write_notification(serde_json::json!({
                        "sessionId": session_id,
                        "update": super::bridge::plan_update(entries.clone()),
                    }));
                } else if let Some(message) = message {
                    self.write_notification(serde_json::json!({
                        "sessionId": session_id,
                        "update": super::bridge::progress_update(
                            "narration",
                            message,
                            None,
                            None,
                            None,
                        ),
                    }));
                } else {
                    self.emit_agent_event_ext(
                        "progress_reported",
                        session_id,
                        serde_json::json!({
                            "message": message,
                            "entries": entries,
                            "replace": replace,
                            "metadata": metadata,
                        }),
                    );
                }
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
                kind,
                cost_usd,
                wall_clock_ms,
            } => {
                let mut payload = serde_json::Map::new();
                payload.insert(
                    "maxIterations".to_string(),
                    serde_json::json!(max_iterations),
                );
                if let Some(kind) = kind {
                    payload.insert("budgetKind".to_string(), serde_json::json!(kind));
                }
                if let Some(cost_usd) = cost_usd {
                    payload.insert("costUsd".to_string(), serde_json::json!(cost_usd));
                }
                if let Some(wall_clock_ms) = wall_clock_ms {
                    payload.insert("wallClockMs".to_string(), serde_json::json!(wall_clock_ms));
                }
                self.emit_agent_event_ext(
                    "budget_exhausted",
                    session_id,
                    serde_json::Value::Object(payload),
                );
            }
            AgentEvent::BudgetCircuitBreaker {
                session_id,
                kind,
                consecutive_count,
                paused_for_ms,
            } => {
                self.emit_agent_event_ext(
                    "budget_circuit_breaker",
                    session_id,
                    serde_json::json!({
                        "breakerKind": kind,
                        "consecutiveCount": consecutive_count,
                        "pausedForMs": paused_for_ms,
                    }),
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
            AgentEvent::AgentLoopStallWarning {
                session_id,
                warning,
            } => {
                self.emit_agent_event_ext("agent_loop_stall_warning", session_id, warning.clone());
            }
            AgentEvent::CapabilityGap {
                session_id,
                level,
                capability,
                provider,
                model,
                fallback_tool_format,
                requested_tool_format,
                message,
            } => {
                let mut payload = serde_json::json!({
                    "level": level,
                    "capability": capability,
                    "provider": provider,
                    "model": model,
                    "fallbackToolFormat": fallback_tool_format,
                    "message": message,
                });
                if let Some(requested) = requested_tool_format {
                    payload["requestedToolFormat"] = serde_json::Value::String(requested.clone());
                }
                self.emit_agent_event_ext("capability_gap", session_id, payload);
            }
            AgentEvent::ToolFormatOverride {
                session_id,
                provider,
                model,
                requested_format,
                recommended_format,
                catalog_parity,
                override_reason,
            } => {
                let mut payload = serde_json::json!({
                    "provider": provider,
                    "model": model,
                    "requestedFormat": requested_format,
                    "recommendedFormat": recommended_format,
                    "catalogParity": catalog_parity,
                });
                if let Some(reason) = override_reason {
                    payload["overrideReason"] = serde_json::Value::String(reason.clone());
                }
                self.emit_agent_event_ext("tool_format_override", session_id, payload);
            }
            AgentEvent::ToolCallAudit {
                session_id,
                tool_call_id,
                tool_name,
                audit,
                receipt,
            } => {
                let mut payload = serde_json::json!({
                    "toolCallId": tool_call_id,
                    "toolName": tool_name,
                    "audit": audit,
                });
                if let Some(receipt) = receipt {
                    payload
                        .as_object_mut()
                        .expect("tool_call_audit payload is an object")
                        .insert(
                            "receipt".to_string(),
                            serde_json::to_value(receipt).expect("receipt serializes"),
                        );
                }
                self.emit_agent_event_ext("tool_call_audit", session_id, payload);
            }
            AgentEvent::CacheHit {
                session_id,
                payload,
                ..
            } => {
                self.emit_agent_event_ext("cache_hit", session_id, payload.clone());
            }
            AgentEvent::CacheMiss {
                session_id,
                payload,
                ..
            } => {
                self.emit_agent_event_ext("cache_miss", session_id, payload.clone());
            }
            AgentEvent::CompositionStart { session_id, run } => {
                self.emit_agent_event_ext(
                    "composition_start",
                    session_id,
                    Self::composition_run_to_json(run),
                );
            }
            AgentEvent::CompositionChildCall { session_id, call } => {
                self.emit_agent_event_ext(
                    "composition_child_call",
                    session_id,
                    Self::composition_child_call_to_json(call),
                );
            }
            AgentEvent::CompositionChildResult { session_id, result } => {
                self.emit_agent_event_ext(
                    "composition_child_result",
                    session_id,
                    Self::composition_child_result_to_json(result),
                );
            }
            AgentEvent::CompositionFinish { session_id, run } => {
                self.emit_agent_event_ext(
                    "composition_finish",
                    session_id,
                    Self::composition_run_to_json(run),
                );
            }
            AgentEvent::CompositionError { session_id, run } => {
                self.emit_agent_event_ext(
                    "composition_error",
                    session_id,
                    Self::composition_run_to_json(run),
                );
            }
            AgentEvent::LoopCheckpoint {
                session_id,
                iteration,
                kind,
                delivered,
                inbox_delivered,
                dispatch_skipped,
            } => {
                let mut payload = serde_json::json!({
                    "iteration": iteration,
                    "kind": kind,
                    "delivered": delivered,
                });
                if *inbox_delivered > 0 {
                    payload["inboxDelivered"] =
                        serde_json::Value::Number(serde_json::Number::from(*inbox_delivered));
                }
                if *dispatch_skipped {
                    payload["dispatchSkipped"] = serde_json::Value::Bool(true);
                }
                self.emit_agent_event_ext("loop_checkpoint", session_id, payload);
            }
        }
    }
}

#[cfg(test)]
mod tests;
