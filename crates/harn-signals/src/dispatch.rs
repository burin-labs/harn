//! Durable monitor pacing. Hosts claim only at a safe dispatch boundary and
//! accept the stable batch id idempotently before committing its acceptance.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::{CaptureIdentity, Config, Inbox, InboxSnapshot, Signal, SignalsError};

pub const DEFAULT_DISPATCH_INTERVAL_SECONDS: u32 = 60;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DispatchBatch {
    pub id: String,
    pub execution_id: String,
    pub occurrences: Vec<Signal>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DispatchClaim {
    pub batch: DispatchBatch,
    pub token: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DispatchPoll {
    pub claim: Option<DispatchClaim>,
    pub pending: usize,
    #[serde(with = "time::serde::rfc3339::option")]
    pub next_due_at: Option<OffsetDateTime>,
}

/// Persisted under the inbox lock. Fields are private so pacing and claim
/// ownership can only change through the transactional API.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DispatchState {
    execution_id: String,
    interval_seconds: u32,
    closed: bool,
    cancelled_monitors: std::collections::BTreeSet<String>,
    retired_executions: std::collections::BTreeSet<String>,
    #[serde(with = "time::serde::rfc3339")]
    observed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    accepted_at: Option<OffsetDateTime>,
    accepted_batch_id: Option<String>,
    claim: Option<DispatchClaim>,
}

pub(crate) fn validate_identity(value: &str) -> Result<(), SignalsError> {
    if value.trim().is_empty() || value.len() > 1024 || value.chars().any(char::is_control) {
        return Err(invalid(
            "dispatch identities must be nonempty, control-free and at most 1024 bytes",
        ));
    }
    Ok(())
}

fn invalid(message: &str) -> SignalsError {
    SignalsError::InvalidInput(message.into())
}

pub(crate) fn validate_capture(
    state: &InboxSnapshot,
    identity: &CaptureIdentity,
) -> Result<(), SignalsError> {
    match (
        state.dispatch.get(&identity.session_id),
        &identity.execution_id,
    ) {
        (None, None) => Ok(()),
        (None, Some(_)) => Err(SignalsError::DispatchForgotten),
        (Some(gate), Some(epoch))
            if !gate.closed
                && gate.execution_id == *epoch
                && !gate.cancelled_monitors.contains(&identity.monitor_key) =>
        {
            Ok(())
        }
        _ => Err(invalid(
            "capture requires the active execution id; cancelled or stale output is refused",
        )),
    }
}

impl Inbox {
    /// Start an execution on a stable host session. Repeating the same epoch
    /// cannot reset its clock or reopen it. Replacing an epoch cancels its
    /// outstanding captures but preserves pacing across execution boundaries.
    pub fn dispatch_start(
        &self,
        session_id: &str,
        execution_id: &str,
        interval_seconds: Option<u32>,
        now: OffsetDateTime,
    ) -> Result<(), SignalsError> {
        validate_identity(session_id)?;
        validate_identity(execution_id)?;
        let interval = interval_seconds.unwrap_or(DEFAULT_DISPATCH_INTERVAL_SECONDS);
        if !(1..=86400).contains(&interval) {
            return Err(invalid(
                "dispatch interval must be between 1 and 86400 seconds",
            ));
        }
        let config = self.config();
        self.transact(now, |state| {
            if let Some(gate) = state.dispatch.get(session_id) {
                if gate.retired_executions.contains(execution_id) {
                    return Err(invalid("a retired execution cannot be reactivated"));
                }
                if gate.execution_id == execution_id {
                    return if !gate.closed && gate.interval_seconds == interval {
                        Ok(())
                    } else {
                        Err(invalid(
                            "an existing epoch cannot be reopened or reconfigured",
                        ))
                    };
                }
                if gate.interval_seconds != interval {
                    return Err(invalid(
                        "a stable session's dispatch interval cannot change across epochs",
                    ));
                }
            }
            make_gate_room(state, session_id, config, now)?;
            cancel_session_captures(state, session_id, now);
            let previous = state.dispatch.remove(session_id);
            let mut retired_executions = previous
                .as_ref()
                .map(|g| g.retired_executions.clone())
                .unwrap_or_default();
            if let Some(gate) = &previous {
                retired_executions.insert(gate.execution_id.clone());
            }
            state.dispatch.insert(
                session_id.into(),
                DispatchState {
                    execution_id: execution_id.into(),
                    interval_seconds: interval,
                    closed: false,
                    cancelled_monitors: Default::default(),
                    retired_executions,
                    observed_at: previous.as_ref().map_or(now, |g| now.max(g.observed_at)),
                    accepted_at: previous.as_ref().and_then(|g| g.accepted_at),
                    accepted_batch_id: previous.and_then(|g| g.accepted_batch_id),
                    claim: None,
                },
            );
            Ok(())
        })?
    }

    /// Claim every eligible captured occurrence as one bounded durable batch.
    /// Call only when the host can dispatch; checking while busy is unnecessary.
    /// One claimant owns the batch until its inbox lease expires. A retry after
    /// expiry gets the same batch id and immutable content with a fresh token.
    pub fn dispatch_claim(
        &self,
        session_id: &str,
        execution_id: &str,
        now: OffsetDateTime,
    ) -> Result<DispatchPoll, SignalsError> {
        let lease_for = self.config().lease_for;
        let config = self.config().clone();
        self.transact(now, |state| {
            let mut ready: Vec<_> = state
                .signals
                .values()
                .filter(|s| {
                    s.is_pending()
                        && s.capture.as_ref().is_some_and(|c| {
                            c.session_id == session_id
                                && c.execution_id.as_deref() == Some(execution_id)
                        })
                        && config.policy.hold(s, now).is_none()
                })
                .collect();
            ready.sort_by_key(|s| s.capture_order);
            let pending = ready.len();
            let oldest = ready.iter().map(|s| s.fired_at).min();
            let gate = state
                .dispatch
                .get_mut(session_id)
                .ok_or(SignalsError::DispatchForgotten)?;
            if gate.closed || gate.execution_id != execution_id {
                return Err(invalid("dispatch epoch is closed or stale"));
            }
            // Wall-clock rollback never advances eligibility. Restart retains
            // this high-water mark instead of reopening a fresh timing window.
            let clock_behind = now < gate.observed_at;
            gate.observed_at = gate.observed_at.max(now);
            if let Some(claim) = &mut gate.claim {
                if claim.expires_at > now || clock_behind {
                    return Ok(DispatchPoll {
                        claim: None,
                        pending,
                        next_due_at: Some(claim.expires_at.max(gate.observed_at)),
                    });
                }
                claim.token = uuid::Uuid::new_v4().simple().to_string();
                claim.expires_at = now + lease_for;
                return Ok(DispatchPoll {
                    claim: Some(claim.clone()),
                    pending,
                    next_due_at: None,
                });
            }
            let interval = Duration::seconds(i64::from(gate.interval_seconds));
            let due = oldest.map(|first| {
                gate.accepted_at
                    .map_or(first + interval, |last| last + interval)
            });
            if due.is_none() || clock_behind || due.is_some_and(|due| due > now) {
                return Ok(DispatchPoll {
                    claim: None,
                    pending,
                    next_due_at: due.map(|due| due.max(gate.observed_at)),
                });
            }
            let occurrences = ready.into_iter().cloned().collect();
            let claim = DispatchClaim {
                batch: DispatchBatch {
                    id: uuid::Uuid::new_v4().simple().to_string(),
                    execution_id: execution_id.into(),
                    occurrences,
                },
                token: uuid::Uuid::new_v4().simple().to_string(),
                expires_at: now + lease_for,
            };
            gate.claim = Some(claim.clone());
            Ok(DispatchPoll {
                claim: Some(claim),
                pending,
                next_due_at: None,
            })
        })?
    }

    /// Commit the host's idempotent acceptance of a batch. This is the pacing
    /// boundary, not signal capture, a poll, or a lease. A crashed host must first
    /// query its durable acceptance by batch id before initiating inference.
    pub fn dispatch_accept(
        &self,
        session_id: &str,
        execution_id: &str,
        batch_id: &str,
        token: &str,
        now: OffsetDateTime,
    ) -> Result<bool, SignalsError> {
        self.transact(now, |state| {
            let gate = state
                .dispatch
                .get_mut(session_id)
                .ok_or(SignalsError::DispatchForgotten)?;
            if gate.closed || gate.execution_id != execution_id {
                return Err(invalid("dispatch epoch is closed or stale"));
            }
            if gate.accepted_batch_id.as_deref() == Some(batch_id) {
                return Ok(false);
            }
            let claim = gate
                .claim
                .as_ref()
                .ok_or_else(|| invalid("no dispatch claim is outstanding"))?;
            if claim.batch.id != batch_id || claim.token != token {
                return Err(invalid("dispatch claim token is stale"));
            }
            let accepted = now.max(gate.observed_at);
            for reminder in &claim.batch.occurrences {
                let signal = state
                    .signals
                    .get_mut(&reminder.key)
                    .ok_or_else(|| invalid("claimed capture is missing"))?;
                if let Some(delivery) = signal.delivery.as_mut() {
                    delivery.acked_at = Some(accepted);
                    delivery.lease = None;
                } else {
                    // A session may go stale during a long host acceptance.
                    signal.delivery = Some(crate::Delivery {
                        generation: signal.generation,
                        session_id: session_id.into(),
                        assigned_at: accepted,
                        lease: None,
                        acked_at: Some(accepted),
                        wakes: 0,
                    });
                }
            }
            gate.observed_at = accepted;
            gate.accepted_at = Some(accepted);
            gate.accepted_batch_id = Some(batch_id.into());
            gate.claim = None;
            Ok(true)
        })?
    }

    /// Cancel one monitor's unaccepted captures, or close the whole epoch.
    /// Invalidating a claim preserves its other monitors for a new batch.
    pub fn dispatch_cancel(
        &self,
        session_id: &str,
        execution_id: &str,
        monitor_key: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<usize, SignalsError> {
        let config = self.config();
        self.transact(now, |state| {
            // A forgotten session already has nothing to cancel.
            let Some(gate) = state.dispatch.get(session_id) else {
                return Ok(0);
            };
            if gate.execution_id != execution_id {
                return Err(invalid("dispatch epoch is stale"));
            }
            if let Some(key) = monitor_key {
                validate_identity(key)?;
                if !gate.cancelled_monitors.contains(key) {
                    make_gate_room(state, session_id, config, now)?;
                }
            }
            let gate = state
                .dispatch
                .get_mut(session_id)
                .ok_or(SignalsError::DispatchForgotten)?;
            if let Some(key) = monitor_key {
                gate.cancelled_monitors.insert(key.into());
            }
            let mut cancelled = 0;
            for signal in state.signals.values_mut() {
                if signal.is_pending()
                    && signal.capture.as_ref().is_some_and(|c| {
                        c.session_id == session_id
                            && c.execution_id.as_deref() == Some(execution_id)
                            && monitor_key.is_none_or(|key| key == c.monitor_key)
                    })
                {
                    signal.cancelled_at = Some(now);
                    signal.delivery = None;
                    cancelled += 1;
                }
            }
            if cancelled > 0 || monitor_key.is_none() {
                gate.claim = None;
            }
            if monitor_key.is_none() {
                gate.closed = true;
            }
            Ok(cancelled)
        })?
    }
}

impl DispatchState {
    pub(crate) fn close(&mut self) {
        self.closed = true;
        self.claim = None;
    }

    /// Forgetting this gate changes nothing a host can observe: no claim is out
    /// and the pacing window is over, so a later start paces the same way.
    fn is_settled(&self, now: OffsetDateTime) -> bool {
        self.claim.is_none() && self.pacing_is_over(now)
    }

    /// A clock behind the high-water mark keeps a gate, since that mark is the
    /// guard.
    fn pacing_is_over(&self, now: OffsetDateTime) -> bool {
        let interval = Duration::seconds(i64::from(self.interval_seconds));
        self.observed_at <= now && self.accepted_at.is_none_or(|last| last + interval <= now)
    }

    fn record_count(&self) -> usize {
        1 + self.cancelled_monitors.len() + self.retired_executions.len()
    }
}

/// The newest pending capture of each session that has one.
fn latest_pending_captures(state: &InboxSnapshot) -> BTreeMap<String, OffsetDateTime> {
    let mut latest = BTreeMap::new();
    for signal in state.signals.values().filter(|signal| signal.is_pending()) {
        if let Some(capture) = &signal.capture {
            latest
                .entry(capture.session_id.clone())
                .and_modify(|at: &mut OffsetDateTime| *at = (*at).max(signal.fired_at))
                .or_insert(signal.fired_at);
        }
    }
    latest
}

fn cancel_session_captures(state: &mut InboxSnapshot, session_id: &str, now: OffsetDateTime) {
    for signal in state.signals.values_mut() {
        if signal.is_pending()
            && signal
                .capture
                .as_ref()
                .is_some_and(|c| c.session_id == session_id)
        {
            signal.cancelled_at = Some(now);
            signal.delivery = None;
        }
    }
}

/// Sessions rarely say they ended. One that has neither polled nor captured for
/// the retention period is treated as dead: its unaccepted captures are
/// cancelled and its gate goes the way a settled signal does. A host that was
/// only silent gets `DispatchForgotten` and resumes with an idempotent start.
pub(crate) fn prune_idle_gates(
    state: &mut InboxSnapshot,
    retain_for: Duration,
    now: OffsetDateTime,
) {
    if !state
        .dispatch
        .values()
        .any(|gate| now - gate.observed_at >= retain_for)
    {
        return;
    }
    let captures = latest_pending_captures(state);
    let dead: Vec<String> = state
        .dispatch
        .iter()
        .filter(|(session_id, gate)| {
            let last_active = captures
                .get(session_id.as_str())
                .map_or(gate.observed_at, |at| gate.observed_at.max(*at));
            now - last_active >= retain_for
                && gate.pacing_is_over(now)
                && gate
                    .claim
                    .as_ref()
                    .is_none_or(|claim| claim.expires_at <= now)
        })
        .map(|(session_id, _)| session_id.clone())
        .collect();
    for session_id in dead {
        cancel_session_captures(state, &session_id, now);
        state.dispatch.remove(&session_id);
    }
}

/// Make room for one more record by forgetting the longest-idle gates of
/// sessions gone quiet for `stale_after`. Nothing is forgotten unless that frees
/// enough, so a refusal leaves every record in place.
fn make_gate_room(
    state: &mut InboxSnapshot,
    starting: &str,
    config: &Config,
    now: OffsetDateTime,
) -> Result<(), SignalsError> {
    let before = metadata_size(state);
    if before < config.max_signals {
        return Ok(());
    }
    let captures = latest_pending_captures(state);
    let mut quiet: Vec<_> = state
        .dispatch
        .iter()
        .filter(|(session_id, gate)| {
            session_id.as_str() != starting
                && now - gate.observed_at >= config.stale_after
                && gate.is_settled(now)
                && !captures.contains_key(session_id.as_str())
        })
        .collect();
    quiet.sort_by_key(|(_, gate)| gate.observed_at);
    let mut remaining = before;
    let mut forgotten = Vec::new();
    for (session_id, gate) in quiet {
        if remaining < config.max_signals {
            break;
        }
        remaining -= gate.record_count();
        forgotten.push(session_id.clone());
    }
    if remaining >= config.max_signals {
        return Err(SignalsError::DispatchFull(before));
    }
    for session_id in forgotten {
        state.dispatch.remove(&session_id);
    }
    Ok(())
}

fn metadata_size(state: &InboxSnapshot) -> usize {
    state
        .dispatch
        .values()
        .map(DispatchState::record_count)
        .sum()
}
