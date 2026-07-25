//! Transcript budget enforcement and recovery.
//!
//! A self-contained slice of the session transcript concern: given a
//! candidate transcript and a session's [`SessionTranscriptBudgetPolicy`],
//! decide whether it fits under the message/event/byte caps and, if not,
//! recover by rejecting, trimming, or LLM-compacting it — attaching an
//! audit event that records the before/after usage. Extracted verbatim
//! from `agent_sessions` (behavior-preserving); the module operates on
//! `VmValue` transcripts and the session's policy, reaching into
//! [`SessionState`] only for the transcript field, the pinned policy, the
//! last-action audit slot, and the id.

use crate::agent_sessions::{
    SessionState, SessionTranscriptBudgetPolicy, TranscriptBudgetRecovery,
};
use crate::value::{VmDictExt, VmValue};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TranscriptBudgetUsage {
    message_count: usize,
    event_count: usize,
    approx_bytes: Option<usize>,
}

fn transcript_messages_from_dict(dict: &crate::value::DictMap) -> Vec<VmValue> {
    match dict.get("messages") {
        Some(VmValue::List(list)) => list.iter().cloned().collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn transcript_message_count(transcript: &VmValue) -> usize {
    transcript
        .as_dict()
        .map(transcript_messages_from_dict)
        .map(|messages| messages.len())
        .unwrap_or(0)
}

fn transcript_events_from_dict(dict: &crate::value::DictMap) -> Vec<VmValue> {
    match dict.get("events") {
        Some(VmValue::List(list)) => list.iter().cloned().collect(),
        _ => {
            let messages = transcript_messages_from_dict(dict);
            crate::llm::helpers::transcript_events_from_messages(&messages)
        }
    }
}

pub(crate) fn transcript_usage(transcript: &VmValue, include_bytes: bool) -> TranscriptBudgetUsage {
    let Some(dict) = transcript.as_dict() else {
        return TranscriptBudgetUsage {
            message_count: 0,
            event_count: 0,
            approx_bytes: include_bytes.then_some(0),
        };
    };
    let approx_bytes = if include_bytes {
        serde_json::to_vec(&crate::llm::helpers::vm_value_to_json(transcript))
            .map(|bytes| bytes.len())
            .ok()
            .or(Some(usize::MAX))
    } else {
        None
    };
    TranscriptBudgetUsage {
        message_count: transcript_messages_from_dict(dict).len(),
        event_count: transcript_events_from_dict(dict).len(),
        approx_bytes,
    }
}

fn transcript_budget_exceeded_reason(
    usage: &TranscriptBudgetUsage,
    policy: &SessionTranscriptBudgetPolicy,
) -> Option<&'static str> {
    if usage.message_count > policy.max_messages {
        return Some("message_count");
    }
    if usage.event_count > policy.max_events {
        return Some("event_count");
    }
    if let (Some(bytes), Some(limit)) = (usage.approx_bytes, policy.max_approx_bytes) {
        if bytes > limit {
            return Some("approx_bytes");
        }
    }
    None
}

pub(crate) fn transcript_budget_usage_json(usage: &TranscriptBudgetUsage) -> serde_json::Value {
    serde_json::json!({
        "messages": usage.message_count,
        "events": usage.event_count,
        "approx_bytes": usage.approx_bytes,
    })
}

pub(crate) fn transcript_budget_policy_json(
    policy: &SessionTranscriptBudgetPolicy,
) -> serde_json::Value {
    let recovery = match &policy.recovery {
        TranscriptBudgetRecovery::Reject => serde_json::json!({"action": "reject"}),
        TranscriptBudgetRecovery::Trim { keep_last } => {
            serde_json::json!({"action": "trim", "keep_last": keep_last})
        }
        TranscriptBudgetRecovery::Compact { keep_last } => {
            serde_json::json!({"action": "compact", "keep_last": keep_last})
        }
    };
    serde_json::json!({
        "max_messages": policy.max_messages,
        "max_events": policy.max_events,
        "max_approx_bytes": policy.max_approx_bytes,
        "recovery": recovery,
    })
}

fn transcript_budget_recovery_name(recovery: &TranscriptBudgetRecovery) -> &'static str {
    match recovery {
        TranscriptBudgetRecovery::Reject => "reject",
        TranscriptBudgetRecovery::Trim { .. } => "trim",
        TranscriptBudgetRecovery::Compact { .. } => "compact",
    }
}

fn transcript_budget_error(
    state: &SessionState,
    policy: &SessionTranscriptBudgetPolicy,
    usage: &TranscriptBudgetUsage,
    reason: &str,
) -> String {
    let byte_suffix = match (usage.approx_bytes, policy.max_approx_bytes) {
        (Some(bytes), Some(limit)) => format!(", approx_bytes {bytes}/{limit}"),
        _ => String::new(),
    };
    format!(
        "transcript budget exceeded for session '{}': {reason} (messages {}/{}, events {}/{}{}; recovery={})",
        state.id,
        usage.message_count,
        policy.max_messages,
        usage.event_count,
        policy.max_events,
        byte_suffix,
        transcript_budget_recovery_name(&policy.recovery),
    )
}

fn transcript_budget_audit_json(
    action: &str,
    source: &str,
    reason: &str,
    policy: &SessionTranscriptBudgetPolicy,
    usage_before: &TranscriptBudgetUsage,
    usage_attempted: &TranscriptBudgetUsage,
    usage_after: &TranscriptBudgetUsage,
) -> serde_json::Value {
    serde_json::json!({
        "action": action,
        "source": source,
        "reason": reason,
        "policy": transcript_budget_policy_json(policy),
        "usage_before": transcript_budget_usage_json(usage_before),
        "usage_attempted": transcript_budget_usage_json(usage_attempted),
        "usage_after": transcript_budget_usage_json(usage_after),
        "removed_messages": usage_attempted.message_count.saturating_sub(usage_after.message_count),
        "removed_events": usage_attempted.event_count.saturating_sub(usage_after.event_count),
    })
}

fn transcript_budget_event(audit: &serde_json::Value) -> VmValue {
    let action = audit
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("enforced");
    crate::llm::helpers::transcript_event(
        "transcript_budget",
        "system",
        "internal",
        &format!("Transcript budget {action}."),
        Some(audit.clone()),
    )
}

fn append_event_to_transcript(transcript: VmValue, event: VmValue) -> VmValue {
    let Some(dict) = transcript.as_dict() else {
        return transcript;
    };
    let mut next = dict.clone();
    let mut events = transcript_events_from_dict(&next);
    events.push(event);
    next.insert(
        crate::value::intern_key("events"),
        VmValue::List(std::sync::Arc::new(events)),
    );
    VmValue::dict(next)
}

fn tail_message_capacity(
    policy: &SessionTranscriptBudgetPolicy,
    reserve_audit_event: bool,
) -> usize {
    let event_capacity = tail_event_capacity(policy, usize::from(reserve_audit_event));
    policy.max_messages.min(event_capacity)
}

fn tail_event_capacity(policy: &SessionTranscriptBudgetPolicy, reserved_events: usize) -> usize {
    policy.max_events.saturating_sub(reserved_events)
}

fn trim_transcript_for_budget(
    transcript: &VmValue,
    policy: &SessionTranscriptBudgetPolicy,
    keep_last: usize,
) -> VmValue {
    let dict = transcript
        .as_dict()
        .cloned()
        .unwrap_or_else(crate::value::DictMap::new);
    let messages = transcript_messages_from_dict(&dict);
    let keep = keep_last.min(tail_message_capacity(policy, true));
    let start = messages.len().saturating_sub(keep);
    let retained: Vec<VmValue> = messages.into_iter().skip(start).collect();
    let mut next = dict;
    next.insert(
        crate::value::intern_key("events"),
        VmValue::List(std::sync::Arc::new(
            crate::llm::helpers::transcript_events_from_messages(&retained),
        )),
    );
    next.insert(
        crate::value::intern_key("messages"),
        VmValue::List(std::sync::Arc::new(retained)),
    );
    next.remove("summary");
    VmValue::dict(next)
}

struct BudgetCompactionLiveEvent {
    receipt: crate::orchestration::CompactionReceipt,
}

struct BudgetCompactionResult {
    transcript: VmValue,
    live_event: Option<BudgetCompactionLiveEvent>,
}

fn compact_transcript_for_budget(
    transcript: &VmValue,
    policy: &SessionTranscriptBudgetPolicy,
    keep_last: usize,
    session_id: &str,
) -> BudgetCompactionResult {
    let dict = transcript
        .as_dict()
        .cloned()
        .unwrap_or_else(crate::value::DictMap::new);
    let messages = transcript_messages_from_dict(&dict);
    let message_capacity = policy.max_messages.min(tail_event_capacity(policy, 2));
    // Auto-compaction may widen a suffix to start on a clean user-turn boundary,
    // so reserve one extra slot beyond the summary when sizing for hard caps.
    let tail_keep = keep_last.min(message_capacity.saturating_sub(2));
    let mut config = crate::orchestration::AutoCompactConfig {
        token_threshold: 0,
        keep_last: tail_keep,
        compact_strategy: crate::orchestration::CompactStrategy::Llm,
        hard_limit_strategy: crate::orchestration::CompactStrategy::Truncate,
        fallback_strategy: Some(crate::orchestration::CompactStrategy::Truncate),
        policy_strategy: crate::orchestration::compact_strategy_name(
            &crate::orchestration::CompactStrategy::Llm,
        )
        .to_string(),
        ..Default::default()
    };

    let mut json_messages = messages
        .iter()
        .map(crate::llm::helpers::vm_value_to_json)
        .collect::<Vec<_>>();
    let lifecycle =
        crate::orchestration::CompactLifecycle::new(crate::orchestration::CompactMode::Auto)
            .with_session_id(Some(session_id))
            .with_trigger(crate::orchestration::CompactionTrigger::BudgetPressure)
            .with_hook_dispatch(false)
            .with_evaluate_providers(false);
    let llm_opts = crate::llm::extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("")),
        VmValue::Nil,
        VmValue::Nil,
    ])
    .ok();
    let outcome = futures::executor::block_on(crate::orchestration::run_compaction_lifecycle(
        &mut json_messages,
        &mut config,
        llm_opts.as_ref(),
        lifecycle,
    ))
    .ok()
    .flatten();

    let retained = json_messages
        .iter()
        .map(crate::stdlib::json_to_vm_value)
        .collect::<Vec<_>>();
    let mut events = crate::llm::helpers::transcript_events_from_messages(&retained);
    let summary = outcome.as_ref().map(|outcome| outcome.summary.clone());
    let mut live_event = None;
    if let Some(outcome) = outcome {
        events.push(crate::llm::helpers::transcript_event_with_id(
            &outcome.receipt.receipt_id,
            "compaction",
            "system",
            "internal",
            "",
            Some(outcome.event_metadata),
        ));
        live_event = Some(BudgetCompactionLiveEvent {
            receipt: outcome.receipt,
        });
    }

    let mut next = dict;
    next.insert(
        crate::value::intern_key("events"),
        VmValue::List(std::sync::Arc::new(events)),
    );
    next.insert(
        crate::value::intern_key("messages"),
        VmValue::List(std::sync::Arc::new(retained)),
    );
    if let Some(summary) = summary {
        next.put_str("summary", summary);
    } else {
        next.remove("summary");
    }
    BudgetCompactionResult {
        transcript: VmValue::dict(next),
        live_event,
    }
}

fn recovered_transcript_with_audit(
    recovered: VmValue,
    action: &str,
    source: &str,
    reason: &str,
    policy: &SessionTranscriptBudgetPolicy,
    usage_before: &TranscriptBudgetUsage,
    usage_attempted: &TranscriptBudgetUsage,
    include_bytes: bool,
) -> (VmValue, serde_json::Value, TranscriptBudgetUsage) {
    let usage_after_without_audit = transcript_usage(&recovered, include_bytes);
    let initial_audit = transcript_budget_audit_json(
        action,
        source,
        reason,
        policy,
        usage_before,
        usage_attempted,
        &usage_after_without_audit,
    );
    let with_initial_audit =
        append_event_to_transcript(recovered.clone(), transcript_budget_event(&initial_audit));
    let usage_after = transcript_usage(&with_initial_audit, include_bytes);
    let audit = transcript_budget_audit_json(
        action,
        source,
        reason,
        policy,
        usage_before,
        usage_attempted,
        &usage_after,
    );
    let with_audit = append_event_to_transcript(recovered, transcript_budget_event(&audit));
    let usage_after = transcript_usage(&with_audit, include_bytes);
    (with_audit, audit, usage_after)
}

pub(crate) fn apply_transcript_with_budget(
    state: &mut SessionState,
    candidate: VmValue,
    source: &str,
) -> Result<(), String> {
    let policy = state.transcript_budget_policy.normalized();
    let include_bytes = policy.max_approx_bytes.is_some();
    let usage_before = transcript_usage(&state.transcript, include_bytes);
    let usage_attempted = transcript_usage(&candidate, include_bytes);
    let Some(reason) = transcript_budget_exceeded_reason(&usage_attempted, &policy) else {
        state.replace_transcript(candidate);
        return Ok(());
    };

    match policy.recovery.clone() {
        TranscriptBudgetRecovery::Reject => {
            let audit = transcript_budget_audit_json(
                "rejected",
                source,
                reason,
                &policy,
                &usage_before,
                &usage_attempted,
                &usage_before,
            );
            state.last_transcript_budget_action = Some(audit);
            Err(transcript_budget_error(
                state,
                &policy,
                &usage_attempted,
                reason,
            ))
        }
        TranscriptBudgetRecovery::Trim { keep_last } => {
            let recovered = trim_transcript_for_budget(&candidate, &policy, keep_last);
            let (with_audit, audit, usage_after) = recovered_transcript_with_audit(
                recovered,
                "trimmed",
                source,
                reason,
                &policy,
                &usage_before,
                &usage_attempted,
                include_bytes,
            );
            if transcript_budget_exceeded_reason(&usage_after, &policy).is_some() {
                let rejected = transcript_budget_audit_json(
                    "rejected",
                    source,
                    reason,
                    &policy,
                    &usage_before,
                    &usage_attempted,
                    &usage_after,
                );
                state.last_transcript_budget_action = Some(rejected);
                return Err(transcript_budget_error(
                    state,
                    &policy,
                    &usage_after,
                    reason,
                ));
            }
            state.last_transcript_budget_action = Some(audit);
            state.replace_transcript(with_audit);
            Ok(())
        }
        TranscriptBudgetRecovery::Compact { keep_last } => {
            let compacted =
                compact_transcript_for_budget(&candidate, &policy, keep_last, &state.id);
            let (with_audit, audit, usage_after) = recovered_transcript_with_audit(
                compacted.transcript,
                "compacted",
                source,
                reason,
                &policy,
                &usage_before,
                &usage_attempted,
                include_bytes,
            );
            if transcript_budget_exceeded_reason(&usage_after, &policy).is_some() {
                let rejected = transcript_budget_audit_json(
                    "rejected",
                    source,
                    reason,
                    &policy,
                    &usage_before,
                    &usage_attempted,
                    &usage_after,
                );
                state.last_transcript_budget_action = Some(rejected);
                return Err(transcript_budget_error(
                    state,
                    &policy,
                    &usage_after,
                    reason,
                ));
            }
            state.last_transcript_budget_action = Some(audit);
            state.replace_transcript(with_audit);
            if let Some(event) = compacted.live_event {
                crate::orchestration::emit_transcript_compacted_event_sync(
                    &state.id,
                    event.receipt,
                );
            }
            Ok(())
        }
    }
}
