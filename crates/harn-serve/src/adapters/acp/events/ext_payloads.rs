//! Typed bodies for `_harn/agentEvent` notifications whose projection is more
//! than a field-for-field copy. Keeping them out of the dispatch match leaves
//! each arm a single readable line.

use super::super::agent_event_payloads as payloads;
use super::AcpAgentEventSink;
use harn_vm::agent_events::AgentEvent;

pub(super) fn subagent_stop(event: &AgentEvent) -> Option<payloads::SubagentStop> {
    let AgentEvent::SubagentStop {
        lineage,
        parent_run_id,
        child_run_id,
        terminal_status,
        terminal_class,
        reason,
        result_ref,
        receipt_ref,
        cancellation,
        timeout,
        completed_at_ms,
        ..
    } = event
    else {
        return None;
    };
    let terminal_status = match super::to_wire(terminal_status) {
        serde_json::Value::String(status) => status,
        other => other.to_string(),
    };
    Some(payloads::SubagentStop {
        parent_run_id: parent_run_id.clone(),
        child_run_id: child_run_id.clone(),
        parent_session_id: lineage
            .as_ref()
            .map(|lineage| lineage.parent.session_id.clone()),
        child_session_id: lineage
            .as_ref()
            .map(|lineage| lineage.child.session_id.clone()),
        terminal_status,
        terminal_class: terminal_class.clone(),
        reason: reason.clone(),
        result_ref: result_ref.clone(),
        receipt_ref: receipt_ref.clone(),
        cancellation: cancellation.clone(),
        timeout: timeout.clone(),
        completed_at_ms: *completed_at_ms,
    })
}

pub(super) fn subagent_join(event: &AgentEvent) -> Option<payloads::SubagentJoin> {
    let AgentEvent::SubagentJoin {
        lineage,
        worker_id,
        completed_at_ms,
        joined_at_ms,
        boundaries,
        ..
    } = event
    else {
        return None;
    };
    Some(payloads::SubagentJoin {
        parent_session_id: lineage.parent.session_id.clone(),
        parent_run_id: lineage.parent.run_id.clone(),
        child_session_id: lineage.child.session_id.clone(),
        child_run_id: lineage.child.run_id.clone(),
        worker_id: worker_id.clone(),
        completed_at_ms: *completed_at_ms,
        joined_at_ms: *joined_at_ms,
        // Renamed from `waitMs`, which reported this same subtraction (#6074).
        collection_lag_ms: joined_at_ms.saturating_sub(*completed_at_ms),
        wait_started_at_ms: boundaries.wait_started_at_ms,
        wait_ms: boundaries.wait_ms(*joined_at_ms),
        result_processing_started_at_ms: boundaries.result_processing_started_at_ms,
        result_processing_completed_at_ms: boundaries.result_processing_completed_at_ms,
        result_processing_ms: boundaries.result_processing_ms(),
    })
}

pub(super) fn missing_tool_call_verdict(
    event: &AgentEvent,
) -> Option<payloads::MissingToolCallVerdict> {
    let AgentEvent::MissingToolCallVerdict {
        iteration,
        action,
        original_action,
        tool_name,
        confidence,
        confidence_threshold,
        evidence,
        language,
        classifier_kind,
        model,
        error,
        ..
    } = event
    else {
        return None;
    };
    Some(payloads::MissingToolCallVerdict {
        iteration: *iteration,
        action: action.clone(),
        original_action: original_action.clone(),
        tool_name: tool_name.clone(),
        confidence: *confidence,
        confidence_threshold: *confidence_threshold,
        evidence: evidence.clone(),
        language: language.clone(),
        classifier_kind: classifier_kind.clone(),
        model: model.clone(),
        error: error.clone(),
    })
}

pub(super) fn emit_documented_stdlib_event(sink: &AcpAgentEventSink, event: &AgentEvent) {
    match event {
        AgentEvent::SubagentJoin { session_id, .. } => {
            if let Some(payload) = subagent_join(event) {
                sink.emit_agent_event(session_id, &payload);
            }
        }
        AgentEvent::SubagentStop { session_id, .. } => {
            if let Some(payload) = subagent_stop(event) {
                sink.emit_agent_event(session_id, &payload);
            }
        }
        AgentEvent::RequireSuccessfulToolsViolation {
            session_id,
            kind,
            source,
            actor,
            run_id,
            redacted_summary,
            recurrence_hints,
            metadata,
        } => sink.emit_agent_event(
            session_id,
            &payloads::RequireSuccessfulToolsViolation {
                violation_kind: kind.clone(),
                source: source.clone(),
                actor: actor.clone(),
                run_id: run_id.clone(),
                redacted_summary: redacted_summary.clone(),
                recurrence_hints: recurrence_hints.clone(),
                metadata: metadata.clone(),
            },
        ),
        AgentEvent::FinalWrapup {
            session_id,
            final_status,
            stop_reason,
            iteration,
            host_directive,
            terminal_kind,
            unconsumed_tool_call,
        } => sink.emit_agent_event(
            session_id,
            &payloads::FinalWrapup {
                final_status: final_status.clone(),
                stop_reason: stop_reason.clone(),
                iteration: *iteration,
                host_directive: *host_directive,
                terminal_kind: terminal_kind.clone(),
                unconsumed_tool_call: unconsumed_tool_call.as_ref().map(|evidence| {
                    payloads::FinalWrapupUnconsumedToolCall {
                        parse_status: match super::to_wire(&evidence.parse_status) {
                            serde_json::Value::String(status) => status,
                            other => other.to_string(),
                        },
                        parsed_call_count: evidence.parsed_call_count,
                        tool_names: evidence.tool_names.clone(),
                        diagnostics: evidence.diagnostics.clone(),
                        evidence_line: evidence.evidence_line.clone(),
                    }
                }),
            },
        ),
        AgentEvent::PackThinkingStripped {
            session_id,
            model,
            requested,
            reason,
        } => sink.emit_agent_event(
            session_id,
            &payloads::PackThinkingStripped {
                model: model.clone(),
                requested: requested.clone(),
                reason: reason.clone(),
            },
        ),
        AgentEvent::SelfConsistencyTie {
            session_id,
            answer,
            total,
            distribution,
        } => sink.emit_agent_event(
            session_id,
            &payloads::SelfConsistencyTie {
                answer: answer.clone(),
                total: *total,
                distribution: distribution.clone(),
            },
        ),
        AgentEvent::CodeLibrarianQueryNlFallback {
            session_id,
            attempted_cypher,
            mcts_depth,
            mcts_expansions,
            result_count,
            text,
        } => sink.emit_agent_event(
            session_id,
            &payloads::CodeLibrarianQueryNlFallback {
                attempted_cypher: attempted_cypher.clone(),
                mcts_depth: *mcts_depth,
                mcts_expansions: *mcts_expansions,
                result_count: *result_count,
                text: text.clone(),
            },
        ),
        AgentEvent::ModelJob { session_id, event } => sink.emit_agent_event(
            session_id,
            &payloads::ModelJob {
                event: event.clone(),
            },
        ),
        _ => unreachable!("emit_documented_stdlib_event called for unrelated event"),
    }
}

/// The loud-boundary funnel (harn#5142). A client that renders this can tell
/// "the model produced nothing" from "the harness dropped what the model
/// produced". `owner` carries the attribution, `boundary` says where,
/// `excerpt` carries the bytes that died.
pub(super) fn boundary_failure(event: &AgentEvent) -> Option<payloads::BoundaryFailure> {
    let AgentEvent::BoundaryFailure {
        boundary,
        kind,
        owner,
        detail,
        excerpt,
        dropped_count,
        dropped_bytes,
        unreported,
        ..
    } = event
    else {
        return None;
    };
    Some(payloads::BoundaryFailure {
        boundary: boundary.as_str().to_string(),
        failure_kind: kind.as_str().to_string(),
        owner: owner.clone(),
        detail: detail.clone(),
        dropped_count: *dropped_count,
        dropped_bytes: *dropped_bytes,
        unreported: *unreported,
        excerpt: excerpt.clone(),
    })
}

/// A concrete provider/model pair lacking a catalog recommendation, and the
/// fallback the runtime chose instead.
pub(super) fn capability_gap(event: &AgentEvent) -> Option<payloads::CapabilityGap> {
    let AgentEvent::CapabilityGap {
        level,
        capability,
        provider,
        model,
        fallback_tool_format,
        requested_tool_format,
        message,
        ..
    } = event
    else {
        return None;
    };
    Some(payloads::CapabilityGap {
        level: level.clone(),
        capability: capability.clone(),
        provider: provider.clone(),
        model: model.clone(),
        fallback_tool_format: fallback_tool_format.clone(),
        message: message.clone(),
        requested_tool_format: requested_tool_format.clone(),
    })
}

/// Middleware audit metadata and its optional privacy-preserving receipt.
pub(super) fn tool_call_audit(event: &AgentEvent) -> Option<payloads::ToolCallAudit> {
    let AgentEvent::ToolCallAudit {
        tool_call_id,
        tool_name,
        audit,
        receipt,
        ..
    } = event
    else {
        return None;
    };
    Some(payloads::ToolCallAudit {
        tool_call_id: tool_call_id.clone(),
        tool_name: tool_name.clone(),
        audit: audit.clone(),
        receipt: receipt.as_ref().map(super::to_wire),
    })
}
