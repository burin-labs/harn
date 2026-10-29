//! Durable monitor pacing. Hosts claim only at a safe dispatch boundary and
//! accept the stable batch id idempotently before committing its acceptance.

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime};

use crate::{CaptureIdentity, Inbox, InboxSnapshot, Signal, SignalsError};

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
        let max_scopes = self.config().max_signals;
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
            if metadata_size(state) >= max_scopes {
                return Err(invalid("dispatch metadata capacity reached"));
            }
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
                .ok_or_else(|| invalid("dispatch session has not started"))?;
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
                .ok_or_else(|| invalid("dispatch session has not started"))?;
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
        let max_monitors = self.config().max_signals;
        self.transact(now, |state| {
            let metadata_count = metadata_size(state);
            let gate = state
                .dispatch
                .get_mut(session_id)
                .ok_or_else(|| invalid("dispatch session has not started"))?;
            if let Some(key) = monitor_key {
                validate_identity(key)?;
                if !gate.cancelled_monitors.contains(key) && metadata_count >= max_monitors {
                    return Err(invalid("cancelled monitor capacity reached"));
                }
            }
            if gate.execution_id != execution_id {
                return Err(invalid("dispatch epoch is stale"));
            }
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
}

fn metadata_size(state: &InboxSnapshot) -> usize {
    state
        .dispatch
        .values()
        .map(|gate| 1 + gate.cancelled_monitors.len() + gate.retired_executions.len())
        .sum()
}
