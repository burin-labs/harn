//! Per-agent autonomy budget tracking and VM-enforced ratification.
//!
//! Mirrors the trigger-side autonomy budget (see `triggers/registry.rs`):
//! when a per-agent_loop call would exceed `per_hour` / `per_day`
//! autonomous-decision quotas, the loop is short-circuited to a
//! waiting-for-approval state with a HITL approval request, a
//! lifecycle event, and a trust-graph tier_transition record from
//! `act_auto` to `act_with_approval`. The script can't bypass the
//! check — enforcement runs in `run_agent_loop_internal` before the
//! first turn, not inside Harn code.

use std::cell::RefCell;
use std::collections::BTreeMap;

use serde_json::json;
use time::OffsetDateTime;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};
use crate::stdlib::hitl::append_approval_request_on;
use crate::trust_graph::{append_trust_record, AutonomyTier, TrustOutcome, TrustRecord};
use crate::value::{VmError, VmValue};

/// Default reviewer when an autonomy-budget exhaustion routes an agent
/// loop to approval. Matches the trigger-side default for consistency.
pub const DEFAULT_AUTONOMY_BUDGET_REVIEWER: &str = "operator";

/// Lifecycle topic for autonomy budget exceedance events. Reuses the
/// trigger-side topic so `harn orchestrator inspect`-style tooling
/// surfaces both forms.
pub const AGENT_AUTONOMY_LIFECYCLE_TOPIC: &str = "triggers.lifecycle";

/// Configuration parsed from the `autonomy_budget:` agent_loop option.
#[derive(Clone, Debug)]
pub struct AgentAutonomyBudget {
    /// Stable identifier used to group decisions across agent_loop
    /// invocations. Defaults to the agent_loop's `session_id`. Choose a
    /// stable key (e.g. persona name) when each invocation creates a
    /// new session — otherwise the budget effectively never accumulates
    /// because each call starts a fresh counter.
    pub key: String,
    /// Maximum autonomous decisions per UTC hour. `None` disables the
    /// hourly cap.
    pub per_hour: Option<u64>,
    /// Maximum autonomous decisions per UTC day. `None` disables the
    /// daily cap.
    pub per_day: Option<u64>,
    /// Reviewer name attached to the HITL approval request when the
    /// budget is exhausted. Defaults to `"operator"`.
    pub reviewer: String,
}

#[derive(Clone, Debug, Default)]
struct CounterEntry {
    day_utc: i32,
    hour_utc: i64,
    count_today: u64,
    count_hour: u64,
}

thread_local! {
    static AUTONOMY_COUNTERS: RefCell<BTreeMap<String, CounterEntry>> = const { RefCell::new(BTreeMap::new()) };
}

/// Reset accumulated counters. Used by `reset_thread_local_state`
/// between test runs so leakage from prior tests can't mask a real
/// regression in budget bookkeeping.
pub fn reset_autonomy_budget_state() {
    AUTONOMY_COUNTERS.with(|slot| slot.borrow_mut().clear());
}

fn utc_day_key() -> i32 {
    (OffsetDateTime::now_utc().date()
        - time::Date::from_calendar_date(1970, time::Month::January, 1).expect("valid epoch date"))
    .whole_days() as i32
}

fn utc_hour_key() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp() / 3_600
}

fn rollover(entry: &mut CounterEntry) {
    let day = utc_day_key();
    let hour = utc_hour_key();
    if entry.day_utc != day {
        entry.day_utc = day;
        entry.count_today = 0;
    }
    if entry.hour_utc != hour {
        entry.hour_utc = hour;
        entry.count_hour = 0;
    }
}

/// Returns the exhaustion reason if dispatching one more autonomous
/// decision under `config` would cross either window's ceiling.
pub fn budget_would_exceed(config: &AgentAutonomyBudget) -> Option<&'static str> {
    AUTONOMY_COUNTERS.with(|slot| {
        let mut counters = slot.borrow_mut();
        let entry = counters.entry(config.key.clone()).or_default();
        rollover(entry);
        if config
            .per_hour
            .is_some_and(|limit| entry.count_hour.saturating_add(1) > limit)
        {
            return Some("hourly_autonomy_budget_exceeded");
        }
        if config
            .per_day
            .is_some_and(|limit| entry.count_today.saturating_add(1) > limit)
        {
            return Some("daily_autonomy_budget_exceeded");
        }
        None
    })
}

/// Increment counters for an autonomous decision. Call exactly once
/// per accepted (non-paused) agent_loop dispatch.
pub fn note_decision(config: &AgentAutonomyBudget) {
    AUTONOMY_COUNTERS.with(|slot| {
        let mut counters = slot.borrow_mut();
        let entry = counters.entry(config.key.clone()).or_default();
        rollover(entry);
        entry.count_hour = entry.count_hour.saturating_add(1);
        entry.count_today = entry.count_today.saturating_add(1);
    });
}

/// Snapshot the current (hour, day) counts for a key. Useful for
/// embedding context in the approval request payload so reviewers can
/// see how many decisions have already fired this window.
pub fn snapshot(key: &str) -> (u64, u64) {
    AUTONOMY_COUNTERS.with(|slot| {
        let mut counters = slot.borrow_mut();
        let entry = counters.entry(key.to_string()).or_default();
        rollover(entry);
        (entry.count_hour, entry.count_today)
    })
}

/// Parse the `autonomy_budget:` option from an agent_loop options dict.
/// Returns `None` when the option is absent or `nil`. The default
/// `key` is provided by the caller (typically the loop's session id).
pub fn parse_autonomy_budget(
    options: Option<&BTreeMap<String, VmValue>>,
    default_key: &str,
    label: &str,
) -> Result<Option<AgentAutonomyBudget>, VmError> {
    let Some(value) = options.and_then(|map| map.get("autonomy_budget")) else {
        return Ok(None);
    };
    let dict = match value {
        VmValue::Nil => return Ok(None),
        VmValue::Dict(d) => d,
        _ => {
            return Err(VmError::Runtime(format!(
                "{label}: autonomy_budget must be a dict {{per_hour, per_day, key?, reviewer?}}"
            )))
        }
    };
    let parse_opt_u64 = |field: &str| -> Result<Option<u64>, VmError> {
        match dict.get(field) {
            None | Some(VmValue::Nil) => Ok(None),
            Some(value) => value
                .as_int()
                .filter(|v| *v >= 0)
                .map(|v| v as u64)
                .map(Some)
                .ok_or_else(|| {
                    VmError::Runtime(format!(
                        "{label}: autonomy_budget.{field} must be a non-negative int"
                    ))
                }),
        }
    };
    let per_hour = parse_opt_u64("per_hour")?;
    let per_day = parse_opt_u64("per_day")?;
    if let Some(0) = per_hour {
        return Err(VmError::Runtime(format!(
            "{label}: autonomy_budget.per_hour must be >= 1 (use nil to disable)"
        )));
    }
    if let Some(0) = per_day {
        return Err(VmError::Runtime(format!(
            "{label}: autonomy_budget.per_day must be >= 1 (use nil to disable)"
        )));
    }
    if per_hour.is_none() && per_day.is_none() {
        // Neither cap configured — treat as "no budget" rather than a
        // silently-permissive entry that still emits trust records.
        return Ok(None);
    }
    let key = match dict.get("key") {
        Some(VmValue::String(s)) if !s.trim().is_empty() => s.to_string(),
        Some(VmValue::Nil) | None => default_key.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{label}: autonomy_budget.key must be a non-empty string, got {}",
                other.type_name()
            )))
        }
    };
    if key.trim().is_empty() {
        return Err(VmError::Runtime(format!(
            "{label}: autonomy_budget requires a non-empty `key` (or a non-anonymous \
             `session_id`) — empty keys would let every anonymous loop share one budget"
        )));
    }
    let reviewer = match dict.get("reviewer") {
        Some(VmValue::String(s)) if !s.trim().is_empty() => s.to_string(),
        Some(VmValue::Nil) | None => DEFAULT_AUTONOMY_BUDGET_REVIEWER.to_string(),
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "{label}: autonomy_budget.reviewer must be a non-empty string, got {}",
                other.type_name()
            )))
        }
    };
    Ok(Some(AgentAutonomyBudget {
        key,
        per_hour,
        per_day,
        reviewer,
    }))
}

/// Outcome of the budget check at agent_loop entry. The `Approved`
/// variant is consumed by `note_decision` (called once the loop has
/// committed to running); the `Denied` variant carries the materialized
/// approval-request id and a result envelope the loop can return as-is.
pub enum BudgetCheckOutcome {
    Approved,
    Denied {
        /// Stable error code indicating which window was exhausted —
        /// `hourly_autonomy_budget_exceeded` or
        /// `daily_autonomy_budget_exceeded`. Embedded in `result`
        /// already; surfaced separately for callers that prefer not to
        /// re-parse the JSON envelope.
        #[allow(dead_code)]
        reason: &'static str,
        /// HITL approval request id the loop emitted to
        /// `hitl.approvals`. Embedded in `result.request_id` already;
        /// surfaced here for callers that need the bare id without
        /// touching the envelope.
        #[allow(dead_code)]
        request_id: String,
        /// JSON envelope the agent_loop returns to the script in lieu
        /// of running. Includes `status: "approval_required"`, the
        /// reason, the request id, and reviewer list.
        result: serde_json::Value,
    },
}

/// Evaluate the budget for `config`, and on exhaustion materialize:
///   1. a HITL approval request (`hitl.approvals` topic),
///   2. a `triggers.lifecycle` event named `autonomy.budget_exceeded`,
///   3. a `trust_graph.records` entry tagged
///      `autonomy.tier_transition` from `act_auto` to
///      `act_with_approval`.
///
/// Returns `BudgetCheckOutcome::Denied` with a JSON envelope the
/// caller should return to the script as the agent_loop result. The
/// envelope includes `status: "approval_required"`, the reason, the
/// request id, and the reviewer list — symmetric with the trigger-side
/// dispatch result.
///
/// On approval, the caller MUST invoke `note_decision` once it has
/// committed to running the loop (after preflight validation passes).
/// Calling `note_decision` before the commit point would burn a slot
/// from the budget for a loop that never actually ran.
pub async fn enforce_budget(
    config: &AgentAutonomyBudget,
    session_id: &str,
    trace_id: &str,
) -> Result<BudgetCheckOutcome, VmError> {
    let Some(reason) = budget_would_exceed(config) else {
        return Ok(BudgetCheckOutcome::Approved);
    };

    let (count_hour, count_today) = snapshot(&config.key);
    let detail = json!({
        "agent_key": config.key,
        "session_id": session_id,
        "reason": reason,
        "from_tier": AutonomyTier::ActAuto.as_str(),
        "requested_tier": AutonomyTier::ActWithApproval.as_str(),
        "per_hour": config.per_hour,
        "per_day": config.per_day,
        "autonomous_decisions_hour": count_hour,
        "autonomous_decisions_today": count_today,
    });
    let reviewers = vec![config.reviewer.clone()];

    // The event_log is required for HITL + trust-graph recording. If
    // the host hasn't installed one (e.g. unit tests that bypass it),
    // we still want the budget check to refuse — but without the audit
    // trail, the failure path is best-effort. Surface as a runtime
    // error so the caller sees the misconfiguration.
    let log = active_event_log().ok_or_else(|| {
        VmError::Runtime(
            "autonomy_budget enforcement requires an active event log; \
             call install_default_for_base_dir before agent_loop"
                .to_string(),
        )
    })?;

    let request_id = append_approval_request_on(
        &log,
        config.key.clone(),
        trace_id.to_string(),
        format!(
            "approve autonomous agent_loop dispatch for '{}' after {}",
            config.key, reason
        ),
        detail.clone(),
        reviewers.clone(),
    )
    .await?;

    let lifecycle_topic = Topic::new(AGENT_AUTONOMY_LIFECYCLE_TOPIC)
        .map_err(|error| VmError::Runtime(error.to_string()))?;
    let lifecycle_payload = json!({
        "agent_key": config.key,
        "session_id": session_id,
        "trace_id": trace_id,
        "reason": reason,
        "request_id": request_id,
        "reviewers": reviewers,
        "from_tier": AutonomyTier::ActAuto.as_str(),
        "requested_tier": AutonomyTier::ActWithApproval.as_str(),
        "per_hour": config.per_hour,
        "per_day": config.per_day,
        "autonomous_decisions_hour": count_hour,
        "autonomous_decisions_today": count_today,
        "source": "agent_loop",
    });
    log.append(
        &lifecycle_topic,
        LogEvent::new("autonomy.budget_exceeded", lifecycle_payload),
    )
    .await
    .map_err(|error| VmError::Runtime(error.to_string()))?;

    let mut record = TrustRecord::new(
        config.key.clone(),
        "autonomy.tier_transition",
        Some(config.reviewer.clone()),
        TrustOutcome::Denied,
        trace_id.to_string(),
        AutonomyTier::ActWithApproval,
    );
    record
        .metadata
        .insert("session_id".to_string(), json!(session_id));
    record.metadata.insert(
        "from_tier".to_string(),
        json!(AutonomyTier::ActAuto.as_str()),
    );
    record.metadata.insert(
        "to_tier".to_string(),
        json!(AutonomyTier::ActWithApproval.as_str()),
    );
    record.metadata.insert("reason".to_string(), json!(reason));
    record
        .metadata
        .insert("request_id".to_string(), json!(request_id));
    record
        .metadata
        .insert("source".to_string(), json!("agent_loop"));
    append_trust_record(&log, &record)
        .await
        .map_err(|error| VmError::Runtime(error.to_string()))?;

    let result = json!({
        "status": "approval_required",
        "approval_required": true,
        "reason": reason,
        "request_id": request_id,
        "reviewers": reviewers,
        "from_tier": AutonomyTier::ActAuto.as_str(),
        "requested_tier": AutonomyTier::ActWithApproval.as_str(),
        "per_hour": config.per_hour,
        "per_day": config.per_day,
        "autonomous_decisions_hour": count_hour,
        "autonomous_decisions_today": count_today,
        "session_id": session_id,
        "agent_key": config.key,
        "llm": {"iterations": 0, "duration_ms": 0, "input_tokens": 0, "output_tokens": 0},
        "tools": {"calls": [], "successful": [], "rejected": [], "mode": ""},
    });

    Ok(BudgetCheckOutcome::Denied {
        reason,
        request_id,
        result,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(key: &str, per_hour: Option<u64>, per_day: Option<u64>) -> AgentAutonomyBudget {
        AgentAutonomyBudget {
            key: key.to_string(),
            per_hour,
            per_day,
            reviewer: DEFAULT_AUTONOMY_BUDGET_REVIEWER.to_string(),
        }
    }

    #[test]
    fn note_decision_increments_until_limit() {
        reset_autonomy_budget_state();
        let cfg = config("agent.alpha", Some(2), Some(10));
        assert!(budget_would_exceed(&cfg).is_none());
        note_decision(&cfg);
        assert!(budget_would_exceed(&cfg).is_none());
        note_decision(&cfg);
        assert_eq!(
            budget_would_exceed(&cfg),
            Some("hourly_autonomy_budget_exceeded")
        );
        let (hour, day) = snapshot("agent.alpha");
        assert_eq!(hour, 2);
        assert_eq!(day, 2);
    }

    #[test]
    fn keys_are_isolated() {
        reset_autonomy_budget_state();
        let alpha = config("agent.alpha", Some(1), None);
        let beta = config("agent.beta", Some(1), None);
        note_decision(&alpha);
        assert_eq!(
            budget_would_exceed(&alpha),
            Some("hourly_autonomy_budget_exceeded")
        );
        assert!(budget_would_exceed(&beta).is_none());
    }

    #[test]
    fn missing_caps_returns_none() {
        let mut opts = BTreeMap::new();
        opts.insert(
            "autonomy_budget".to_string(),
            VmValue::Dict(std::rc::Rc::new(BTreeMap::new())),
        );
        let parsed = parse_autonomy_budget(Some(&opts), "session-1", "agent_loop").expect("parses");
        assert!(parsed.is_none());
    }

    #[test]
    fn rejects_zero_caps() {
        let mut inner = BTreeMap::new();
        inner.insert("per_hour".to_string(), VmValue::Int(0));
        let mut opts = BTreeMap::new();
        opts.insert(
            "autonomy_budget".to_string(),
            VmValue::Dict(std::rc::Rc::new(inner)),
        );
        let err = parse_autonomy_budget(Some(&opts), "session-1", "agent_loop")
            .expect_err("zero per_hour rejected");
        assert!(matches!(err, VmError::Runtime(msg) if msg.contains("per_hour must be >= 1")));
    }

    #[test]
    fn rejects_empty_default_key_when_caps_set() {
        let mut inner = BTreeMap::new();
        inner.insert("per_hour".to_string(), VmValue::Int(3));
        let mut opts = BTreeMap::new();
        opts.insert(
            "autonomy_budget".to_string(),
            VmValue::Dict(std::rc::Rc::new(inner)),
        );
        let err = parse_autonomy_budget(Some(&opts), "", "agent_loop")
            .expect_err("empty default key with caps must reject");
        assert!(matches!(err, VmError::Runtime(msg) if msg.contains("non-empty `key`")));
    }

    #[test]
    fn parses_full_dict() {
        let mut inner = BTreeMap::new();
        inner.insert("per_hour".to_string(), VmValue::Int(3));
        inner.insert("per_day".to_string(), VmValue::Int(20));
        inner.insert(
            "key".to_string(),
            VmValue::String(std::rc::Rc::from("captain.persona")),
        );
        inner.insert(
            "reviewer".to_string(),
            VmValue::String(std::rc::Rc::from("oncall")),
        );
        let mut opts = BTreeMap::new();
        opts.insert(
            "autonomy_budget".to_string(),
            VmValue::Dict(std::rc::Rc::new(inner)),
        );
        let parsed = parse_autonomy_budget(Some(&opts), "ignored-default", "agent_loop")
            .expect("parses")
            .expect("present");
        assert_eq!(parsed.per_hour, Some(3));
        assert_eq!(parsed.per_day, Some(20));
        assert_eq!(parsed.key, "captain.persona");
        assert_eq!(parsed.reviewer, "oncall");
    }
}
