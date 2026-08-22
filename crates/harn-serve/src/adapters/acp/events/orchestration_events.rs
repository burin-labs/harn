//! Orchestration and diagnostic projections behind the canonical ACP event sink.

use super::super::event_projection::has_progress_entries;
use super::{ext_payloads, AcpAgentEventSink};
use harn_vm::agent_events::AgentEvent;

pub(super) fn handle(sink: &AcpAgentEventSink, event: &AgentEvent) {
    match event {
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
                    "update": super::super::bridge::plan_update(entries.clone()),
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
                sink.emit_agent_event_ext(
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
            let mut payload = serde_json::json!({
                "toolCallId": tool_call_id,
                "mode": mode,
                "action": action,
                "persona": persona,
                "originalTool": original_tool,
                "routedTool": routed_tool,
                "targetTool": target_tool,
            });
            if let Some(path) = path {
                payload["path"] = serde_json::Value::String(path.clone());
            }
            sink.emit_agent_event_ext("compass_routing_decision", session_id, payload);
        }
        AgentEvent::AgentScratchpadReorganization {
            session_id,
            iteration,
            status,
            details,
        } => {
            sink.emit_agent_event_ext(
                "agent_scratchpad_reorganization",
                session_id,
                serde_json::json!({
                    "iteration": iteration,
                    "status": status,
                    "details": details,
                }),
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
        } => {
            let mut payload = serde_json::json!({"feedbackKind": kind, "content": content});
            if let Some(object) = payload.as_object_mut() {
                if let Some(streak) = streak {
                    object.insert("streak".to_string(), serde_json::json!(streak));
                }
                if let Some(iteration) = iteration {
                    object.insert("iteration".to_string(), serde_json::json!(iteration));
                }
                if let Some(tool_name) = tool_name {
                    object.insert("toolName".to_string(), serde_json::json!(tool_name));
                }
                if let Some(claimed) = turn_claimed_for_repair {
                    object.insert(
                        "turnClaimedForRepair".to_string(),
                        serde_json::json!(claimed),
                    );
                }
            }
            sink.emit_agent_event_ext("feedback_injected", session_id, payload);
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
            sink.emit_agent_event_ext(
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
            sink.emit_agent_event_ext(
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
            sink.emit_agent_event_ext(
                "loop_stuck",
                session_id,
                serde_json::json!({
                    "maxNudges": max_nudges,
                    "lastIteration": last_iteration,
                    "tailExcerpt": tail_excerpt,
                }),
            );
        }
        AgentEvent::LoopStuckSignal {
            session_id,
            payload,
        } => {
            sink.emit_agent_event_ext("loop_stuck", session_id, payload.clone());
        }
        AgentEvent::ReservedTerminalVerify {
            session_id,
            payload,
        } => {
            sink.emit_agent_event_ext("reserved_terminal_verify", session_id, payload.clone());
        }
        AgentEvent::DaemonWatchdogTripped {
            session_id,
            attempts,
            elapsed_ms,
        } => {
            sink.emit_agent_event_ext(
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
            sink.emit_agent_event_ext(
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
            sink.emit_agent_event_ext("agent_loop_stall_warning", session_id, warning.clone());
        }
        AgentEvent::CapabilityGap { session_id, .. } => {
            sink.emit_agent_event_ext(
                "capability_gap",
                session_id,
                ext_payloads::capability_gap(event),
            );
        }
        AgentEvent::BoundaryFailure { session_id, .. } => {
            sink.emit_agent_event_ext(
                "boundary_failure",
                session_id,
                ext_payloads::boundary_failure(event),
            );
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
            sink.emit_agent_event_ext("tool_format_override", session_id, payload);
        }
        AgentEvent::ToolCallAudit { session_id, .. } => {
            sink.emit_agent_event_ext(
                "tool_call_audit",
                session_id,
                ext_payloads::tool_call_audit(event),
            );
        }
        AgentEvent::ToolBatchDisposition {
            session_id,
            receipt,
        } => {
            sink.emit_agent_event_ext(
                "tool_batch_disposition",
                session_id,
                serde_json::json!({"receipt": receipt}),
            );
        }
        AgentEvent::CompositionStart { session_id, run } => {
            sink.emit_agent_event_ext(
                "composition_start",
                session_id,
                AcpAgentEventSink::composition_run_to_json(run),
            );
        }
        AgentEvent::CompositionChildCall { session_id, call } => {
            sink.emit_agent_event_ext(
                "composition_child_call",
                session_id,
                AcpAgentEventSink::composition_child_call_to_json(call),
            );
        }
        AgentEvent::CompositionChildResult { session_id, result } => {
            sink.emit_agent_event_ext(
                "composition_child_result",
                session_id,
                AcpAgentEventSink::composition_child_result_to_json(result),
            );
        }
        AgentEvent::CompositionFinish { session_id, run } => {
            sink.emit_agent_event_ext(
                "composition_finish",
                session_id,
                AcpAgentEventSink::composition_run_to_json(run),
            );
        }
        AgentEvent::CompositionError { session_id, run } => {
            sink.emit_agent_event_ext(
                "composition_error",
                session_id,
                AcpAgentEventSink::composition_run_to_json(run),
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
            let mut payload = serde_json::json!({
                "iteration": iteration,
                "kind": kind,
                "delivered": delivered,
            });
            if *inbox_delivered > 0 {
                payload["inboxDelivered"] =
                    serde_json::Value::Number(serde_json::Number::from(*inbox_delivered));
            }
            if *typed_delivered > 0 {
                payload["typedDelivered"] =
                    serde_json::Value::Number(serde_json::Number::from(*typed_delivered));
            }
            if *dispatch_skipped {
                payload["dispatchSkipped"] = serde_json::Value::Bool(true);
            }
            sink.emit_agent_event_ext("loop_checkpoint", session_id, payload);
        }
        AgentEvent::McpNotification {
            session_id,
            server,
            method,
            direction,
            params,
        } => {
            sink.emit_agent_event_ext(
                "mcp_notification",
                session_id,
                serde_json::json!({
                    "server": server,
                    "method": method,
                    "direction": direction,
                    "params": params,
                }),
            );
        }
        AgentEvent::McpCatalogChanged {
            session_id,
            server,
            reason,
        } => {
            sink.emit_agent_event_ext(
                "mcp_catalog_changed",
                session_id,
                serde_json::json!({
                    "server": server,
                    "reason": reason,
                }),
            );
        }
        AgentEvent::McpAuthRequired {
            session_id,
            server,
            resource,
            scope,
        } => {
            sink.emit_agent_event_ext(
                "mcp_auth_required",
                session_id,
                serde_json::json!({
                    "server": server,
                    "resource": resource,
                    "scope": scope,
                }),
            );
        }
        AgentEvent::OrchestrationDecision {
            session_id,
            decision,
        } => {
            sink.emit_agent_event_ext(
                "orchestration_decision",
                session_id,
                serde_json::json!({"decision": decision}),
            );
        }
        _ => unreachable!("event extension dispatcher received an unsupported event"),
    }
}
