//! Payload bodies for `_harn/agentEvent` extension notifications.
//!
//! ACP defines no `session/update` discriminator for Harn-native milestones, so
//! they ride the `_harn/agentEvent` extension channel. Each builder here
//! produces that notification's `params` body minus the `sessionId` / `kind`
//! keys, which `emit_agent_event_ext` stamps on. Keeping them out of the
//! dispatch match leaves each arm a single readable line and gives the wire
//! shapes one owner.

use harn_vm::agent_events::AgentEvent;

pub(super) fn subagent_stop(event: &AgentEvent) -> serde_json::Value {
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
        return serde_json::json!({});
    };
    serde_json::json!({
        "parentRunId": parent_run_id,
        "childRunId": child_run_id,
        "parentSessionId": lineage.as_ref().map(|lineage| &lineage.parent.session_id),
        "childSessionId": lineage.as_ref().map(|lineage| &lineage.child.session_id),
        "terminalStatus": terminal_status,
        "terminalClass": terminal_class,
        "reason": reason,
        "resultRef": result_ref,
        "receiptRef": receipt_ref,
        "cancellation": cancellation,
        "timeout": timeout,
        "completedAtMs": completed_at_ms,
    })
}

pub(super) fn subagent_join(event: &AgentEvent) -> serde_json::Value {
    let AgentEvent::SubagentJoin {
        lineage,
        worker_id,
        completed_at_ms,
        joined_at_ms,
        boundaries,
        ..
    } = event
    else {
        return serde_json::json!({});
    };
    serde_json::json!({
        "parentSessionId": lineage.parent.session_id,
        "parentRunId": lineage.parent.run_id,
        "childSessionId": lineage.child.session_id,
        "childRunId": lineage.child.run_id,
        "workerId": worker_id,
        "completedAtMs": completed_at_ms,
        "joinedAtMs": joined_at_ms,
        // Renamed from `waitMs`, which reported this same subtraction (#6074).
        // It is the collection lag, not the parent's wait: the parent may have
        // been waiting long before the child reached a terminal state, and
        // `waitMs` below is now that number.
        "collectionLagMs": joined_at_ms.saturating_sub(*completed_at_ms),
        "waitStartedAtMs": boundaries.wait_started_at_ms,
        "waitMs": boundaries.wait_ms(*joined_at_ms),
        "resultProcessingStartedAtMs": boundaries.result_processing_started_at_ms,
        "resultProcessingCompletedAtMs": boundaries.result_processing_completed_at_ms,
        "resultProcessingMs": boundaries.result_processing_ms(),
    })
}

pub(super) fn missing_tool_call_verdict(event: &AgentEvent) -> serde_json::Value {
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
        return serde_json::json!({});
    };
    serde_json::json!({
        "iteration": iteration,
        "action": action,
        "originalAction": original_action,
        "toolName": tool_name,
        "confidence": confidence,
        "confidenceThreshold": confidence_threshold,
        "evidence": evidence,
        "language": language,
        "classifierKind": classifier_kind,
        "model": model,
        "error": error,
    })
}

pub(super) fn documented_stdlib_event(
    event: &AgentEvent,
) -> (&'static str, &str, serde_json::Value) {
    match event {
        AgentEvent::SubagentJoin { session_id, .. } => {
            ("subagent_join", session_id, subagent_join(event))
        }
        AgentEvent::SubagentStop { session_id, .. } => {
            ("subagent_stop", session_id, subagent_stop(event))
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
        } => (
            "require_successful_tools_violation",
            session_id,
            serde_json::json!({
                "violationKind": kind,
                "source": source,
                "actor": actor,
                "runId": run_id,
                "redactedSummary": redacted_summary,
                "recurrenceHints": recurrence_hints,
                "metadata": metadata,
            }),
        ),
        AgentEvent::FinalWrapup {
            session_id,
            final_status,
            stop_reason,
            iteration,
            host_directive,
            terminal_kind,
            unconsumed_tool_call,
        } => (
            "final_wrapup",
            session_id,
            serde_json::json!({
                "finalStatus": final_status,
                "stopReason": stop_reason,
                "iteration": iteration,
                "hostDirective": host_directive,
                "terminalKind": terminal_kind,
                "unconsumedToolCall": unconsumed_tool_call.as_ref().map(|evidence| serde_json::json!({
                    "parseStatus": evidence.parse_status,
                    "parsedCallCount": evidence.parsed_call_count,
                    "toolNames": evidence.tool_names,
                    "diagnostics": evidence.diagnostics,
                    "evidenceLine": evidence.evidence_line,
                })),
            }),
        ),
        AgentEvent::PackThinkingStripped {
            session_id,
            model,
            requested,
            reason,
        } => (
            "pack_thinking_stripped",
            session_id,
            serde_json::json!({
                "model": model,
                "requested": requested,
                "reason": reason,
            }),
        ),
        AgentEvent::SelfConsistencyTie {
            session_id,
            answer,
            total,
            distribution,
        } => (
            "self_consistency_tie",
            session_id,
            serde_json::json!({
                "answer": answer,
                "total": total,
                "distribution": distribution,
            }),
        ),
        AgentEvent::CodeLibrarianQueryNlFallback {
            session_id,
            attempted_cypher,
            mcts_depth,
            mcts_expansions,
            result_count,
            text,
        } => (
            "code_librarian_query_nl_fallback",
            session_id,
            serde_json::json!({
                "attemptedCypher": attempted_cypher,
                "mctsDepth": mcts_depth,
                "mctsExpansions": mcts_expansions,
                "resultCount": result_count,
                "text": text,
            }),
        ),
        AgentEvent::ModelJob { session_id, event } => {
            ("model_job", session_id, serde_json::json!({"event": event}))
        }
        _ => unreachable!("documented_stdlib_event called for unrelated event"),
    }
}

/// The loud-boundary funnel (harn#5142). A client that renders this can tell
/// "the model produced nothing" from "the harness dropped what the model
/// produced" — the distinction the whole class of bug turned on. `owner`
/// carries the attribution, `boundary` says where, `excerpt` carries the bytes
/// that died.
pub(super) fn boundary_failure(event: &AgentEvent) -> serde_json::Value {
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
        return serde_json::json!({});
    };
    let mut payload = serde_json::json!({
        "boundary": boundary.as_str(),
        "failureKind": kind.as_str(),
        "owner": owner,
        "detail": detail,
        "droppedCount": dropped_count,
        "droppedBytes": dropped_bytes,
        "unreported": unreported,
    });
    if let Some(excerpt) = excerpt {
        payload["excerpt"] = serde_json::Value::String(excerpt.clone());
    }
    payload
}

/// A concrete provider/model pair lacking a catalog recommendation, and the
/// fallback the runtime chose instead.
pub(super) fn capability_gap(event: &AgentEvent) -> serde_json::Value {
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
        return serde_json::json!({});
    };
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
    payload
}

/// Middleware audit metadata and its optional privacy-preserving receipt.
pub(super) fn tool_call_audit(event: &AgentEvent) -> serde_json::Value {
    let AgentEvent::ToolCallAudit {
        tool_call_id,
        tool_name,
        audit,
        receipt,
        ..
    } = event
    else {
        return serde_json::json!({});
    };
    let mut payload = serde_json::json!({
        "toolCallId": tool_call_id,
        "toolName": tool_name,
        "audit": audit,
    });
    if let Some(receipt) = receipt {
        payload["receipt"] = serde_json::to_value(receipt).expect("receipt serializes");
    }
    payload
}
