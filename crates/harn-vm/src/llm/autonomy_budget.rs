//! Per-agent autonomy budget tracking and VM-enforced ratification.
//!
//! This mirrors the trigger-side autonomy budget for `agent_loop` calls:
//! when a per-loop autonomous-decision quota is exhausted, the loop
//! short-circuits before any LLM call and emits the same approval-shaped
//! result the trigger dispatcher uses.

use std::cell::RefCell;
use std::collections::BTreeMap;

use serde_json::json;
use time::OffsetDateTime;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};
use crate::stdlib::hitl::append_approval_request_on;
use crate::trust_graph::{append_trust_record, AutonomyTier, TrustOutcome, TrustRecord};
use crate::value::{VmError, VmValue};

pub const DEFAULT_AUTONOMY_BUDGET_REVIEWER: &str = "operator";
pub const AGENT_AUTONOMY_LIFECYCLE_TOPIC: &str = "triggers.lifecycle";

#[derive(Clone, Debug)]
pub(crate) struct AgentAutonomyBudget {
    pub(crate) key: String,
    pub(crate) per_hour: Option<u64>,
    pub(crate) per_day: Option<u64>,
    pub(crate) reviewer: String,
}

#[derive(Clone, Debug, Default)]
struct CounterEntry {
    day_utc: i32,
    hour_utc: i64,
    count_today: u64,
    count_hour: u64,
}

thread_local! {
    static AUTONOMY_COUNTERS: RefCell<BTreeMap<String, CounterEntry>> =
        const { RefCell::new(BTreeMap::new()) };
}

pub(crate) fn reset_autonomy_budget_state() {
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

pub(crate) fn budget_would_exceed(config: &AgentAutonomyBudget) -> Option<&'static str> {
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

pub(crate) fn note_decision(config: &AgentAutonomyBudget) {
    AUTONOMY_COUNTERS.with(|slot| {
        let mut counters = slot.borrow_mut();
        let entry = counters.entry(config.key.clone()).or_default();
        rollover(entry);
        entry.count_hour = entry.count_hour.saturating_add(1);
        entry.count_today = entry.count_today.saturating_add(1);
    });
}

pub(crate) fn snapshot(key: &str) -> (u64, u64) {
    AUTONOMY_COUNTERS.with(|slot| {
        let mut counters = slot.borrow_mut();
        let entry = counters.entry(key.to_string()).or_default();
        rollover(entry);
        (entry.count_hour, entry.count_today)
    })
}

pub(crate) fn parse_autonomy_budget(
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
            "{label}: autonomy_budget requires a non-empty `key` or session_id"
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

pub(crate) enum BudgetCheckOutcome {
    Approved(AgentAutonomyBudget),
    Denied { result: serde_json::Value },
}

pub(crate) async fn enforce_budget(
    config: AgentAutonomyBudget,
    session_id: &str,
    trace_id: &str,
) -> Result<BudgetCheckOutcome, VmError> {
    let Some(reason) = budget_would_exceed(&config) else {
        return Ok(BudgetCheckOutcome::Approved(config));
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
    let log = active_event_log().ok_or_else(|| {
        VmError::Runtime(
            "autonomy_budget enforcement requires an active event log before agent_loop"
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
        detail,
        reviewers.clone(),
    )
    .await?;

    let lifecycle_topic = Topic::new(AGENT_AUTONOMY_LIFECYCLE_TOPIC)
        .map_err(|error| VmError::Runtime(error.to_string()))?;
    log.append(
        &lifecycle_topic,
        LogEvent::new(
            "autonomy.budget_exceeded",
            json!({
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
            }),
        ),
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

    Ok(BudgetCheckOutcome::Denied {
        result: json!({
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
        }),
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
        let parsed = parse_autonomy_budget(Some(&opts), "session-1", "agent_loop").expect("parse");
        assert!(parsed.is_none());
    }
}
