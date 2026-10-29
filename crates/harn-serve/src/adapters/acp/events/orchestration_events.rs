//! Orchestration and diagnostic projections behind the canonical ACP event sink.

use super::super::agent_event_payloads as payloads;
use super::super::event_projection::has_progress_entries;
use super::{ext_payloads, AcpAgentEventSink};
use harn_vm::agent_events::AgentEvent;

pub(super) fn handle(sink: &AcpAgentEventSink, event: &AgentEvent) {
    match event {
        AgentEvent::PurposeLabel {
            session_id,
            label,
            source,
            iteration,
            tool_call_ids,
            tool_call_count,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::PurposeLabel {
                    label: label.clone(),
                    source: match super::to_wire(source) {
                        serde_json::Value::String(source) => source,
                        other => other.to_string(),
                    },
                    iteration: *iteration,
                    tool_call_ids: tool_call_ids.clone(),
                    tool_call_count: *tool_call_count,
                },
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
                sink.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": super::super::bridge::acp_plan_update(entries.clone()),
                }));
            } else if let Some(message) = message {
                sink.write_notification(serde_json::json!({
                    "sessionId": session_id,
                    "update": super::super::bridge::progress_update(
                        "narration",
                        message,
                        None,
                        None,
                        None,
                    ),
                }));
            } else {
                sink.emit_agent_event(
                    session_id,
                    &payloads::ProgressReported {
                        message: message.clone(),
                        entries: entries.clone(),
                        replace: *replace,
                        metadata: metadata.clone(),
                    },
                );
            }
        }
        AgentEvent::CompassRoutingDecision {
            session_id,
            tool_call_id,
            mode,
            action,
            persona,
            original_tool,
            routed_tool,
            target_tool,
            path,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::CompassRoutingDecision {
                    tool_call_id: tool_call_id.clone(),
                    mode: mode.clone(),
                    action: action.clone(),
                    persona: persona.clone(),
                    original_tool: original_tool.clone(),
                    routed_tool: routed_tool.clone(),
                    target_tool: target_tool.clone(),
                    path: path.clone(),
                },
            );
        }
        AgentEvent::AgentScratchpadReorganization {
            session_id,
            iteration,
            status,
            details,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::AgentScratchpadReorganization {
                    iteration: *iteration,
                    status: status.clone(),
                    details: details.clone(),
                },
            );
        }
        AgentEvent::FeedbackInjected {
            session_id,
            kind,
            content,
            streak,
            iteration,
            tool_name,
            turn_claimed_for_repair,
            delivered,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::FeedbackInjected {
                    feedback_kind: kind.clone(),
                    content: content.clone(),
                    streak: *streak,
                    iteration: *iteration,
                    tool_name: tool_name.clone(),
                    turn_claimed_for_repair: *turn_claimed_for_repair,
                    delivered: *delivered,
                },
            );
        }
        AgentEvent::BudgetExhausted {
            session_id,
            max_iterations,
            iteration,
            kind,
            cost_usd,
            wall_clock_ms,
            limit,
            limit_value,
            projected_cost_usd,
            session_cost_usd,
            projected_input_tokens,
            projected_output_tokens,
            projection_basis,
            headroom_usd,
            provider,
            model,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::BudgetExhausted {
                    max_iterations: *max_iterations,
                    iteration: *iteration,
                    budget_kind: kind.clone(),
                    cost_usd: *cost_usd,
                    wall_clock_ms: *wall_clock_ms,
                    limit: limit.clone(),
                    limit_value: limit_value.clone(),
                    projected_cost_usd: *projected_cost_usd,
                    session_cost_usd: *session_cost_usd,
                    projected_input_tokens: *projected_input_tokens,
                    projected_output_tokens: *projected_output_tokens,
                    projection_basis: projection_basis.clone(),
                    headroom_usd: *headroom_usd,
                    provider: provider.clone(),
                    model: model.clone(),
                },
            );
        }
        AgentEvent::BudgetCircuitBreaker {
            session_id,
            kind,
            consecutive_count,
            paused_for_ms,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::BudgetCircuitBreaker {
                    breaker_kind: kind.clone(),
                    consecutive_count: *consecutive_count,
                    paused_for_ms: *paused_for_ms,
                },
            );
        }
        AgentEvent::LoopStuck {
            session_id,
            max_nudges,
            last_iteration,
            tail_excerpt,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::LoopStuck {
                    max_nudges: Some(*max_nudges),
                    last_iteration: Some(*last_iteration),
                    tail_excerpt: Some(tail_excerpt.clone()),
                    signal: serde_json::Map::new(),
                },
            );
        }
        AgentEvent::LoopStuckSignal {
            session_id,
            payload,
        } => {
            let signal = match payload {
                serde_json::Value::Object(fields) => fields.clone(),
                _ => serde_json::Map::new(),
            };
            sink.emit_agent_event(
                session_id,
                &payloads::LoopStuck {
                    signal,
                    ..payloads::LoopStuck::default()
                },
            );
        }
        AgentEvent::ReservedTerminalVerify {
            session_id,
            payload,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::ReservedTerminalVerify::from_value(payload.clone()),
            );
        }
        AgentEvent::DaemonWatchdogTripped {
            session_id,
            attempts,
            elapsed_ms,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::DaemonWatchdogTripped {
                    attempts: *attempts,
                    elapsed_ms: *elapsed_ms,
                },
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
            sink.emit_agent_event(
                session_id,
                &payloads::LoopControlDecision {
                    iteration: *iteration,
                    action: action.clone(),
                    old_limit: *old_limit,
                    new_limit: *new_limit,
                    reason: reason.clone(),
                    status: status.clone(),
                },
            );
        }
        AgentEvent::AgentLoopStallWarning {
            session_id,
            warning,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::AgentLoopStallWarning::from_value(warning.clone()),
            );
        }
        AgentEvent::CapabilityGap { session_id, .. } => {
            if let Some(payload) = ext_payloads::capability_gap(event) {
                sink.emit_agent_event(session_id, &payload);
            }
        }
        AgentEvent::BoundaryFailure { session_id, .. } => {
            if let Some(payload) = ext_payloads::boundary_failure(event) {
                sink.emit_agent_event(session_id, &payload);
            }
        }
        AgentEvent::ToolFormatOverride {
            session_id,
            provider,
            model,
            requested_format,
            recommended_format,
            catalog_parity,
            override_reason,
            applied_format,
            steered,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::ToolFormatOverride {
                    provider: provider.clone(),
                    model: model.clone(),
                    requested_format: requested_format.clone(),
                    recommended_format: recommended_format.clone(),
                    catalog_parity: catalog_parity.clone(),
                    override_reason: override_reason.clone(),
                    applied_format: applied_format.clone(),
                    steered: *steered,
                },
            );
        }
        AgentEvent::ToolCallAudit { session_id, .. } => {
            if let Some(payload) = ext_payloads::tool_call_audit(event) {
                sink.emit_agent_event(session_id, &payload);
            }
        }
        AgentEvent::ToolBatchDisposition {
            session_id,
            receipt,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::ToolBatchDisposition {
                    receipt: super::to_wire(receipt),
                },
            );
        }
        AgentEvent::CompositionStart { session_id, run } => {
            sink.emit_agent_event(
                session_id,
                &payloads::CompositionStart(payloads::CompositionRun::new(run)),
            );
        }
        AgentEvent::CompositionChildCall { session_id, call } => {
            sink.emit_agent_event(
                session_id,
                &payloads::CompositionChildCall {
                    run_id: call.run_id.clone(),
                    tool_call_id: call.tool_call_id.clone(),
                    tool_name: call.tool_name.clone(),
                    operation_index: call.operation_index,
                    annotations: call.annotations.as_ref().map(super::to_wire),
                    requested_side_effect_level: call
                        .requested_side_effect_level
                        .as_str()
                        .to_string(),
                    policy_context: call.policy_context.clone(),
                    raw_input: call.raw_input.clone(),
                },
            );
        }
        AgentEvent::CompositionChildResult { session_id, result } => {
            sink.emit_agent_event(
                session_id,
                &payloads::CompositionChildResult {
                    run_id: result.run_id.clone(),
                    tool_call_id: result.tool_call_id.clone(),
                    tool_name: result.tool_name.clone(),
                    operation_index: result.operation_index,
                    status: AcpAgentEventSink::status_str(result.status).to_string(),
                    raw_output: result.raw_output.clone(),
                    error: result.error.clone(),
                    error_category: result
                        .error_category
                        .map(|category| category.as_str().to_string()),
                    executor: result
                        .executor
                        .as_ref()
                        .map(AcpAgentEventSink::executor_to_json),
                    duration_ms: result.duration_ms,
                    execution_duration_ms: result.execution_duration_ms,
                },
            );
        }
        AgentEvent::CompositionFinish { session_id, run } => {
            sink.emit_agent_event(
                session_id,
                &payloads::CompositionFinish(payloads::CompositionRun::new(run)),
            );
        }
        AgentEvent::CompositionError { session_id, run } => {
            sink.emit_agent_event(
                session_id,
                &payloads::CompositionError(payloads::CompositionRun::new(run)),
            );
        }
        AgentEvent::LoopCheckpoint {
            session_id,
            iteration,
            kind,
            delivered,
            inbox_delivered,
            typed_delivered,
            dispatch_skipped,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::LoopCheckpoint {
                    iteration: *iteration,
                    checkpoint_kind: kind.clone(),
                    delivered: *delivered,
                    inbox_delivered: *inbox_delivered,
                    typed_delivered: *typed_delivered,
                    dispatch_skipped: *dispatch_skipped,
                },
            );
        }
        AgentEvent::McpNotification {
            session_id,
            server,
            method,
            direction,
            params,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::McpNotification {
                    server: server.clone(),
                    method: method.clone(),
                    direction: direction.clone(),
                    params: params.clone(),
                },
            );
        }
        AgentEvent::McpCatalogChanged {
            session_id,
            server,
            reason,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::McpCatalogChanged {
                    server: server.clone(),
                    reason: reason.clone(),
                },
            );
        }
        AgentEvent::McpAuthRequired {
            session_id,
            server,
            resource,
            scope,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::McpAuthRequired {
                    server: server.clone(),
                    resource: resource.clone(),
                    scope: scope.clone(),
                },
            );
        }
        AgentEvent::OrchestrationDecision {
            session_id,
            decision,
        } => {
            sink.emit_agent_event(
                session_id,
                &payloads::OrchestrationDecision {
                    decision: decision.clone(),
                },
            );
        }
        _ => unreachable!("event extension dispatcher received an unsupported event"),
    }
}
