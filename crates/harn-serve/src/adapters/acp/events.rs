//! ACP AgentEventSink — translates canonical `AgentEvent` variants into ACP
//! `session/update` notifications. Registered per-session at prompt start.

use super::event_projection::passthrough_projection;
use super::AcpOutput;
use harn_vm::agent_events::{AgentEvent, AgentEventSink, ToolExecutor};
use harn_vm::composition::{CompositionChildCall, CompositionChildResult, CompositionRunEnvelope};
use harn_vm::visible_text::sanitize_visible_assistant_text;

/// Writes canonical ACP `session/update` notifications for each `AgentEvent`.
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

    /// Emit a Harn agent event as `_harn/agentEvent`. The schema is the per-kind
    /// payload defined in `HARN_AGENT_EVENT_KINDS`: top-level `sessionId` and
    /// `kind`, plus kind-specific fields directly under `params`.
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

impl AgentEventSink for AcpAgentEventSink {
    fn handle_event(&self, event: &AgentEvent) {
        match event {
            // Diagnostics that differ only in wire name and carry their whole
            // emitted record. `event_projection` owns that table; listing the
            // family here keeps the match exhaustive, so a new variant must
            // still make a deliberate choice rather than hit a catch-all.
            event @ (AgentEvent::CacheHit { .. }
            | AgentEvent::CacheMiss { .. }
            | AgentEvent::LlmCallLog { .. }
            | AgentEvent::LlmRoutingDecision { .. }
            | AgentEvent::LlmFallbackAttempt { .. }
            | AgentEvent::LlmShadowDiff { .. }
            | AgentEvent::SemanticCacheHit { .. }
            | AgentEvent::SemanticCacheMiss { .. }) => {
                if let Some((kind, payload)) = passthrough_projection(event) {
                    self.emit_agent_event_ext(kind, event.session_id(), payload.clone());
                }
            }
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
                mutation_status,
                changed_paths,
                data,
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
            AgentEvent::PlanDocumentUpdated { session_id, event } => {
                let document = event.document();
                let update = serde_json::json!({
                    "sessionUpdate": "plan",
                    "entries": harn_vm::llm::plan::plan_document_entries(document),
                    "harnPlanDocument": document,
                });
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
                let mut harn_meta = serde_json::Map::new();
                let mut put = |key: &str, value: serde_json::Value| {
                    harn_meta.insert(key.to_string(), value);
                };
                // `receiptId` + `schemaVersion` give ACP consumers the stable
                // runtime identity so they no longer synthesize a host-local
                // UUID (harn#4995).
                put("receiptId", receipt.receipt_id.clone().into());
                put("schemaVersion", receipt.schema_version.into());
                put("mode", receipt.mode.clone().into());
                put("reason", receipt.reason.clone().into());
                put("strategy", receipt.strategy.clone().into());
                put("engineStrategy", receipt.engine_strategy.clone().into());
                put("archivedMessages", receipt.archived_messages.into());
                put(
                    "estimatedTokensBefore",
                    receipt.estimated_tokens_before.into(),
                );
                put(
                    "estimatedTokensAfter",
                    receipt.estimated_tokens_after.into(),
                );
                put(
                    "snapshotAssetId",
                    receipt
                        .snapshot_asset_id
                        .clone()
                        .map_or(serde_json::Value::Null, serde_json::Value::String),
                );
                if let Some(instruction_mode) = &receipt.instruction_mode {
                    put("instructionMode", instruction_mode.clone().into());
                }
                if let Some(instruction_source) = &receipt.instruction_source {
                    put("instructionSource", instruction_source.clone().into());
                }
                if let Some(compaction_policy) = &receipt.compaction_policy {
                    put("compactionPolicy", compaction_policy.clone());
                }
                if let Some(recap) = &receipt.recap {
                    put(
                        "recap",
                        serde_json::to_value(recap).unwrap_or(serde_json::Value::Null),
                    );
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
            AgentEvent::JudgeStarted {
                session_id,
                iteration,
                trigger,
            } => {
                self.emit_agent_event_ext(
                    "judge_started",
                    session_id,
                    serde_json::json!({
                        "iteration": iteration,
                        "trigger": trigger,
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
                source,
                trigger,
                reason,
                confirm,
                converted_from,
                escalation_recommended,
                escalation_target,
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
                        "source": source,
                        "trigger": trigger,
                        "reason": reason,
                        "confirm": confirm,
                        "convertedFrom": converted_from,
                        "escalationRecommended": escalation_recommended,
                        "escalationTarget": escalation_target,
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
                judge_error,
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
                        "judgeError": judge_error,
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
            } => {
                self.emit_agent_event_ext(
                    "input_guardrail_verdict",
                    session_id,
                    serde_json::json!({
                        "iteration": iteration,
                        "tripwire": tripwire,
                        "reason": reason,
                        "label": label,
                        "confidence": confidence,
                        "confidenceThreshold": confidence_threshold,
                        "classifierKind": classifier_kind,
                        "model": model,
                        "error": error,
                    }),
                );
            }
            AgentEvent::MissingToolCallVerdict { session_id, .. } => self.emit_agent_event_ext(
                "missing_tool_call_verdict",
                session_id,
                ext_payloads::missing_tool_call_verdict(event),
            ),
            AgentEvent::RepairOutputContractApplied {
                session_id,
                iteration,
                tool_count,
            } => self.emit_agent_event_ext(
                "repair_output_contract_applied",
                session_id,
                serde_json::json!({
                    "iteration": iteration,
                    "toolCount": tool_count,
                }),
            ),
            AgentEvent::SubagentJoin { .. }
            | AgentEvent::SubagentStop { .. }
            | AgentEvent::RequireSuccessfulToolsViolation { .. }
            | AgentEvent::FinalWrapup { .. }
            | AgentEvent::PackThinkingStripped { .. }
            | AgentEvent::SelfConsistencyTie { .. }
            | AgentEvent::CodeLibrarianQueryNlFallback { .. }
            | AgentEvent::ModelJob { .. } => {
                let (kind, session_id, payload) = ext_payloads::documented_stdlib_event(event);
                self.emit_agent_event_ext(kind, session_id, payload);
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
            } => {
                self.emit_agent_event_ext(
                    "host_tool_result",
                    session_id,
                    serde_json::json!({
                        "injectionId": injection_id,
                        "toolCallId": tool_call_id,
                        "toolName": tool_name,
                        "kind": kind,
                        "rawInput": raw_input,
                        "status": Self::status_str(*status),
                        "rawOutput": raw_output,
                        "resultPointer": result_pointer,
                        "error": error,
                        "durationMs": duration_ms,
                        "delivery": delivery,
                        "deliveredAtSeam": delivered_at_seam,
                        "sequence": sequence,
                        "provenance": provenance,
                        "sanitization": sanitization,
                    }),
                );
            }
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
            } => {
                self.emit_agent_event_ext(
                    "host_attachment",
                    session_id,
                    serde_json::json!({
                        "injectionId": injection_id,
                        "mediaType": media_type,
                        "flavor": flavor,
                        "artifactPointer": artifact_pointer,
                        "sha256": sha256,
                        "sizeBytes": size_bytes,
                        "rendered": rendered,
                        "description": description,
                        "descriptionModel": description_model,
                        "delivery": delivery,
                        "deliveredAtSeam": delivered_at_seam,
                        "sequence": sequence,
                        "provenance": provenance,
                        "sanitization": sanitization,
                    }),
                );
            }
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
            event @ (AgentEvent::ProgressReported { .. }
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

pub(super) use meta::merge_harn_meta;
