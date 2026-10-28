//! The durable per-machine inbox: one JSON document under an advisory lock.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use harn_flock::{lock_with_deadline, LockMode};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::config::Config;
use crate::event::TriageEvent;
use crate::model::{
    Activity, CaptureIdentity, CheckIn, CheckInScope, Delivery, Lease, Reminder, ReminderId,
    Session, SessionAnnouncement, Signal, SignalInput, SignalState, SignatureStatus,
};
use crate::router::{Router, RuleRouter};
use crate::watcher::{self, WatcherLock};
use crate::SignalsError;

pub const INBOX_SCHEMA: &str = "harn.signals.inbox.v2";
pub const SIGNALS_DIR_ENV: &str = "HARN_EXT_SIGNALS_DIR";

const INBOX_FILE: &str = "inbox.json";
const LOCK_FILE: &str = "inbox.lock";
/// Transactions are milliseconds; a wait this long means a wedged holder. It
/// stays well under a harness's hook timeout, so a hook fails on its own terms.
const LOCK_DEADLINE: Duration = Duration::from_secs(2);
/// A check-in inside this window only refreshes `last_seen` in memory, so a
/// burst of tool calls does not rewrite the file on every hook.
const HEARTBEAT_GRANULARITY: time::Duration = time::Duration::seconds(10);
/// Wakes an idle watcher spends on one delivery. A reminder the woken turn
/// never leases (its `Stop` hook is missing or failing) stops waking the
/// session after this many tries instead of waking it forever.
pub const MAX_WAKES_PER_REMINDER: u32 = 3;

/// Everything in the inbox, as stored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct InboxSnapshot {
    pub signals: BTreeMap<String, Signal>,
    pub sessions: BTreeMap<String, Session>,
}

#[derive(Serialize, Deserialize)]
struct Document {
    schema: String,
    #[serde(flatten)]
    state: InboxSnapshot,
}

/// What a fire or resolve did to the signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Transition {
    /// A real state change; one reminder is owed for this generation.
    Changed { state: SignalState, generation: u64 },
    /// Same state as before. Content was refreshed; no reminder is owed.
    Unchanged { state: SignalState, generation: u64 },
    /// No signal has this key.
    Unknown,
}

/// What an idle watcher's poll found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdlePoll {
    /// The urgent reminders waiting for this session with no lease out and
    /// wakes left; possibly none yet. The watcher only wakes the session: the session's own next
    /// check-in leases and renders them.
    Pending(Vec<ReminderId>),
    /// The session is working again; its foreground hooks deliver now.
    Busy,
    /// The session ended or went stale.
    Gone,
}

pub struct Inbox {
    dir: PathBuf,
    config: Config,
    router: Box<dyn Router>,
}

impl Inbox {
    /// `$HARN_EXT_SIGNALS_DIR`, else `~/.harn/signals`.
    pub fn default_dir() -> Result<PathBuf, SignalsError> {
        if let Some(dir) = std::env::var_os(SIGNALS_DIR_ENV).filter(|value| !value.is_empty()) {
            return Ok(PathBuf::from(dir));
        }
        std::env::home_dir()
            .map(|home| home.join(".harn").join("signals"))
            .ok_or(SignalsError::NoHome)
    }

    /// Open (creating if needed) the inbox in `dir`, reading `config.toml`
    /// there when present.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, SignalsError> {
        let dir = dir.into();
        create_private_dir(&dir)?;
        let config = Config::load(&dir)?;
        let router = Box::new(RuleRouter {
            fallback_role: config.fallback_role.clone(),
        });
        Ok(Self {
            dir,
            config,
            router,
        })
    }

    /// Replace the routing rules, for example with a triage pipeline.
    pub fn with_router(mut self, router: impl Router + 'static) -> Self {
        self.router = Box::new(router);
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Fire a signal. Firing one that is already fired extends its expiry and
    /// owes no reminder; its text stays frozen for the generation, so every
    /// redelivery reads the same. A higher severity is a new generation.
    pub fn fire(
        &self,
        input: SignalInput,
        now: OffsetDateTime,
    ) -> Result<Transition, SignalsError> {
        self.fire_inner(input, None, now)
    }

    /// Capture a distinct output occurrence. Retrying the same session,
    /// monitor key and event id is idempotent for the retained record's lifetime.
    /// Use a file cursor or producer sequence, not output text, as `event_id`.
    pub fn capture(
        &self,
        session_id: &str,
        event_id: &str,
        mut input: SignalInput,
        now: OffsetDateTime,
    ) -> Result<Transition, SignalsError> {
        let monitor_key = input
            .key
            .take()
            .unwrap_or_else(|| input.event.dedupe_key.clone());
        for value in [session_id, event_id, monitor_key.as_str()] {
            if value.trim().is_empty() || value.len() > 1024 || value.chars().any(char::is_control)
            {
                return Err(SignalsError::InvalidInput(
                    "capture identities must be nonempty, control-free and at most 1024 bytes"
                        .into(),
                ));
            }
        }
        if input.event.summary.chars().count() > crate::MAX_SUMMARY_CHARS {
            return Err(SignalsError::InvalidInput("capture summary exceeds the storage bound; capture smaller chunks or reference the full output".into()));
        }
        let identity = CaptureIdentity {
            session_id: session_id.into(),
            monitor_key,
            event_id: event_id.into(),
        };
        let encoded = serde_json::to_vec(&identity)
            .map_err(|error| SignalsError::InvalidInput(error.to_string()))?;
        input.key = Some(format!(
            "capture:{}",
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, &encoded).simple()
        ));
        self.fire_inner(input, Some(identity), now)
    }

    fn fire_inner(
        &self,
        input: SignalInput,
        capture: Option<CaptureIdentity>,
        now: OffsetDateTime,
    ) -> Result<Transition, SignalsError> {
        let SignalInput {
            key,
            mut event,
            severity,
            routing,
            provenance,
            expires_at,
        } = input;
        event.prepare_for_storage()?;
        let key = key.unwrap_or_else(|| event.dedupe_key.clone());
        if capture.is_none() && key.starts_with("capture:") {
            return Err(SignalsError::InvalidInput(
                "capture: keys are reserved for captured occurrences".into(),
            ));
        }
        if key.trim().is_empty() {
            return Err(SignalsError::InvalidInput("signal key is empty".into()));
        }
        // Keys are identities built from source text; a line break or escape
        // sequence in one could forge lines wherever the key is shown.
        if key.chars().any(char::is_control) {
            return Err(SignalsError::InvalidInput(
                "signal key contains a control character".into(),
            ));
        }
        if provenance.producer.trim().is_empty() {
            return Err(SignalsError::InvalidInput(
                "signal provenance names no producer".into(),
            ));
        }
        let expires_at = Some(expires_at.unwrap_or(now + self.config.fired_ttl));
        let max_signals = self.config.max_signals;
        self.transact(now, |state| {
            if let Some(signal) = state.signals.get(&key) {
                refuse_weaker(signal, provenance.signature)?;
                if signal.capture != capture {
                    return Err(SignalsError::InvalidInput(
                        "capture identity conflicts with the stored occurrence".into(),
                    ));
                }
                if capture.is_some() {
                    return Ok(Transition::Unchanged {
                        state: signal.state,
                        generation: signal.generation,
                    });
                }
            }
            let signal = match state.signals.get_mut(&key) {
                Some(signal) => signal,
                None => {
                    make_room(state, max_signals)?;
                    let capture_order = if capture.is_some() {
                        Some(
                            state
                                .signals
                                .values()
                                .filter_map(|signal| signal.capture_order)
                                .max()
                                .unwrap_or(0)
                                .checked_add(1)
                                .ok_or_else(|| {
                                    SignalsError::InvalidInput(
                                        "capture ordering counter exhausted".into(),
                                    )
                                })?,
                        )
                    } else {
                        None
                    };
                    state.signals.insert(
                        key.clone(),
                        Signal {
                            capture,
                            capture_order,
                            key: key.clone(),
                            state: SignalState::Fired,
                            generation: 1,
                            severity,
                            event,
                            routing,
                            provenance,
                            fired_at: now,
                            changed_at: now,
                            expires_at,
                            delivery: None,
                            attempts: 0,
                            fired_recipient: None,
                        },
                    );
                    return Ok(Transition::Changed {
                        state: SignalState::Fired,
                        generation: 1,
                    });
                }
            };
            signal.expires_at = expires_at;
            let escalated = severity > signal.severity;
            if signal.state == SignalState::Fired && !escalated {
                return Ok(Transition::Unchanged {
                    state: SignalState::Fired,
                    generation: signal.generation,
                });
            }
            if signal.state != SignalState::Fired {
                signal.fired_recipient = None;
                signal.fired_at = now;
            }
            signal.event = event;
            signal.severity = severity;
            signal.routing = routing;
            signal.provenance = provenance;
            Ok(advance(signal, SignalState::Fired, now))
        })?
    }

    /// Resolve a fired signal, optionally replacing its content with the
    /// recovery event. Resolving anything else changes nothing. `signature`
    /// is how the resolving report was checked; it may not be weaker than the
    /// signal's own.
    pub fn resolve(
        &self,
        key: &str,
        event: Option<TriageEvent>,
        signature: SignatureStatus,
        now: OffsetDateTime,
    ) -> Result<Transition, SignalsError> {
        let event = match event {
            Some(mut event) => {
                event.prepare_for_storage()?;
                Some(event)
            }
            None => None,
        };
        self.transact(now, |state| {
            let Some(signal) = state.signals.get_mut(key) else {
                return Ok(Transition::Unknown);
            };
            refuse_weaker(signal, signature)?;
            if signal.capture.is_some() {
                return Err(SignalsError::InvalidInput(
                    "captured occurrences cannot be resolved; acknowledge their delivery".into(),
                ));
            }
            if signal.state != SignalState::Fired {
                return Ok(Transition::Unchanged {
                    state: signal.state,
                    generation: signal.generation,
                });
            }
            if let Some(event) = event {
                signal.event = event;
            }
            Ok(advance(signal, SignalState::Resolved, now))
        })?
    }

    /// Record a session's presence without leasing or acking anything.
    pub fn register_session(
        &self,
        announcement: SessionAnnouncement,
        now: OffsetDateTime,
    ) -> Result<(), SignalsError> {
        self.transact(now, |state| upsert_session(state, announcement, now))
    }

    /// Remove a session. Its unacked reminders, leased or not, go back to the
    /// router; a lease the session never acked is redelivered elsewhere.
    pub fn end_session(&self, session_id: &str, now: OffsetDateTime) -> Result<(), SignalsError> {
        self.transact(now, |state| {
            state.sessions.remove(session_id);
        })
    }

    /// Check in: record the session's presence and lease up to `limit`
    /// pending reminders in `scope`.
    ///
    /// Delivery is at least once. The caller hands each leased reminder to
    /// the model, then acks it with [`Inbox::ack_leases`] and the lease's
    /// token; nothing else acks a lease, so one caller never acks what another
    /// caller leased. A lease that lapses unacked is leased again with the
    /// next attempt number: to its holder while that session is responsive,
    /// otherwise to another one. Callers render each leased reminder by its
    /// [`crate::ReminderId`], so a redelivery reads the same.
    pub fn check_in(
        &self,
        announcement: SessionAnnouncement,
        scope: CheckInScope,
        limit: Option<usize>,
        now: OffsetDateTime,
    ) -> Result<CheckIn, SignalsError> {
        let session_id = announcement.id.clone();
        let lease_for = self.config.lease_for;
        self.transact_routed(now, |state, router, config| {
            upsert_session(state, announcement, now);
            route_pending(state, router, config, now);
            let (reminders, waiting) = lease(state, &session_id, scope, limit, lease_for, now);
            CheckIn { reminders, waiting }
        })
    }

    /// Ack the leases a check-in handed out, by token, once their reminders
    /// reached the model. A token that no longer names the reminder's current
    /// lease (it lapsed and was leased again, moved to another session, or
    /// the signal changed generation) acks nothing. Returns the ids acked.
    pub fn ack_leases(
        &self,
        session_id: &str,
        tokens: &[String],
        now: OffsetDateTime,
    ) -> Result<Vec<ReminderId>, SignalsError> {
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        self.transact(now, |state| {
            let mut acked = Vec::new();
            for signal in state.signals.values_mut() {
                let id = signal.id();
                let Some(delivery) = signal.delivery.as_mut() else {
                    continue;
                };
                let owned = delivery.session_id == session_id
                    && delivery.acked_at.is_none()
                    && delivery
                        .lease
                        .as_ref()
                        .is_some_and(|lease| tokens.contains(&lease.token));
                if owned {
                    delivery.acked_at = Some(now);
                    delivery.lease = None;
                    acked.push(id);
                }
            }
            acked
        })
    }

    /// Take `session_id`'s idle-watcher lock without waiting, or `None` when
    /// another watcher holds it. Dropping the lock removes its file.
    pub fn claim_watcher(&self, session_id: &str) -> Result<Option<WatcherLock>, SignalsError> {
        watcher::claim(&self.dir, session_id)
    }

    /// One poll of an idle session's background watcher: refresh its
    /// heartbeat and count the urgent reminders routed to it, without leasing
    /// them and without overwriting the activity a foreground hook recorded
    /// since the watcher started.
    pub fn poll_idle(
        &self,
        session_id: &str,
        now: OffsetDateTime,
    ) -> Result<IdlePoll, SignalsError> {
        self.transact(now, |state| {
            let Some(session) = state.sessions.get_mut(session_id) else {
                return IdlePoll::Gone;
            };
            if session.activity != Activity::Idle {
                return IdlePoll::Busy;
            }
            if now - session.last_seen >= HEARTBEAT_GRANULARITY {
                session.last_seen = now;
            }
            let pending: Vec<ReminderId> = state
                .signals
                .values()
                .filter(|signal| {
                    signal.is_pending()
                        && CheckInScope::UrgentOnly.admits(signal.severity)
                        && signal.delivery.as_ref().is_some_and(|delivery| {
                            delivery.session_id == session_id
                                && !delivery.lease_active(now)
                                && delivery.wakes < MAX_WAKES_PER_REMINDER
                        })
                })
                .map(Signal::id)
                .collect();
            IdlePoll::Pending(pending)
        })
    }

    /// Count one wake of `session_id` against each of these reminders.
    pub fn record_wake(
        &self,
        session_id: &str,
        ids: &[ReminderId],
        now: OffsetDateTime,
    ) -> Result<(), SignalsError> {
        self.transact(now, |state| {
            for id in ids {
                let Some(signal) = state.signals.get_mut(&id.key) else {
                    continue;
                };
                if signal.generation != id.generation {
                    continue;
                }
                if let Some(delivery) = signal
                    .delivery
                    .as_mut()
                    .filter(|delivery| delivery.session_id == session_id)
                {
                    delivery.wakes = delivery.wakes.saturating_add(1);
                }
            }
        })
    }

    /// Ack leased reminders explicitly. `session_id`, when given, must be the
    /// recipient. Returns the ids that were acked; an id whose generation has
    /// moved on, was never leased, or is already acked is skipped.
    pub fn ack(
        &self,
        session_id: Option<&str>,
        ids: &[ReminderId],
        now: OffsetDateTime,
    ) -> Result<Vec<ReminderId>, SignalsError> {
        self.transact(now, |state| {
            let mut acked = Vec::new();
            for id in ids {
                let Some(signal) = state.signals.get_mut(&id.key) else {
                    continue;
                };
                if signal.generation != id.generation || signal.attempts == 0 {
                    continue;
                }
                let Some(delivery) = signal.delivery.as_mut() else {
                    continue;
                };
                if delivery.acked_at.is_some()
                    || delivery.lease.is_none()
                    || session_id.is_some_and(|session| session != delivery.session_id)
                {
                    continue;
                }
                delivery.acked_at = Some(now);
                delivery.lease = None;
                acked.push(id.clone());
            }
            acked
        })
    }

    /// The inbox after expiry, pruning, and routing have been applied.
    pub fn snapshot(&self, now: OffsetDateTime) -> Result<InboxSnapshot, SignalsError> {
        self.transact(now, |state| state.clone())
    }

    fn transact<T>(
        &self,
        now: OffsetDateTime,
        apply: impl FnOnce(&mut InboxSnapshot) -> T,
    ) -> Result<T, SignalsError> {
        self.transact_routed(now, |state, _, _| apply(state))
    }

    /// Lock, read, maintain, apply, route, and write only if anything changed.
    fn transact_routed<T>(
        &self,
        now: OffsetDateTime,
        apply: impl FnOnce(&mut InboxSnapshot, &dyn Router, &Config) -> T,
    ) -> Result<T, SignalsError> {
        let lock_path = self.dir.join(LOCK_FILE);
        let lock = open_private(&lock_path)?;
        lock_with_deadline(&lock, &lock_path, LockMode::Exclusive, LOCK_DEADLINE)?;
        let path = self.dir.join(INBOX_FILE);
        let before = read_document(&path)?;
        let mut state = before.clone();
        maintain(&mut state, &self.config, now);
        route_pending(&mut state, self.router.as_ref(), &self.config, now);
        let result = apply(&mut state, self.router.as_ref(), &self.config);
        route_pending(&mut state, self.router.as_ref(), &self.config, now);
        if state != before {
            let sessions: Vec<String> = state.sessions.keys().cloned().collect();
            write_document(&path, state)?;
            // Still under the lock, so no inbox write is in flight. A live
            // session's watcher lock is left to its watcher and `SessionEnd`.
            sweep_debris(&self.dir, &sessions);
        }
        Ok(result)
    }
}

fn advance(signal: &mut Signal, next: SignalState, now: OffsetDateTime) -> Transition {
    signal.state = next;
    signal.generation += 1;
    signal.changed_at = now;
    signal.delivery = None;
    signal.attempts = 0;
    Transition::Changed {
        state: next,
        generation: signal.generation,
    }
}

fn upsert_session(
    state: &mut InboxSnapshot,
    announcement: SessionAnnouncement,
    now: OffsetDateTime,
) {
    let SessionAnnouncement {
        id,
        harness,
        cwd,
        repo,
        title,
        role,
        activity,
    } = announcement;
    match state.sessions.get_mut(&id) {
        Some(session) => {
            let unchanged = session.harness == harness
                && session.cwd == cwd
                && session.repo == repo
                && (title.is_none() || session.title == title)
                && session.role == role
                && session.activity == activity;
            if unchanged && now - session.last_seen < HEARTBEAT_GRANULARITY {
                return;
            }
            session.harness = harness;
            session.cwd = cwd;
            session.repo = repo;
            if title.is_some() {
                session.title = title;
            }
            session.role = role;
            session.activity = activity;
            session.last_seen = now;
        }
        None => {
            state.sessions.insert(
                id.clone(),
                Session {
                    id,
                    harness,
                    cwd,
                    repo,
                    title,
                    role,
                    activity,
                    registered_at: now,
                    last_seen: now,
                },
            );
        }
    }
}

/// Expire lapsed signals, prune settled ones past retention, and drop stale
/// sessions.
fn maintain(state: &mut InboxSnapshot, config: &Config, now: OffsetDateTime) {
    for signal in state.signals.values_mut() {
        if signal.state == SignalState::Fired
            && signal
                .expires_at
                .is_some_and(|expires_at| expires_at <= now)
        {
            advance(signal, SignalState::Expired, now);
        }
    }
    state.signals.retain(|_, signal| {
        signal.state == SignalState::Fired || now - signal.changed_at < config.retain_for
    });
    state
        .sessions
        .retain(|_, session| now - session.last_seen <= config.stale_after);
}

/// Give every pending generation a live recipient the policy allows.
///
/// An assignment stays put while its lease is out or its holder is
/// responsive, so a reminder does not bounce between sessions; a responsive
/// holder whose lease lapsed is leased it again at its next check-in. A holder
/// silent for a lease length (crashed, or deep in one long tool call) loses
/// a reminder with no lease out to a responsive session when the router finds
/// one. An assignment the policy now holds is withdrawn once no lease is out.
fn route_pending(
    state: &mut InboxSnapshot,
    router: &dyn Router,
    config: &Config,
    now: OffsetDateTime,
) {
    let live: Vec<&Session> = state
        .sessions
        .values()
        .filter(|session| now - session.last_seen <= config.stale_after)
        .collect();
    let responsive = |session: &Session| now - session.last_seen < config.lease_for;
    for signal in state.signals.values_mut() {
        if !signal.is_pending() {
            continue;
        }
        let held = config.policy.hold(signal, now).is_some();
        if let Some(capture) = &signal.capture {
            if signal
                .delivery
                .as_ref()
                .is_some_and(|delivery| delivery.lease_active(now))
            {
                continue;
            }
            let owner_live = live.iter().any(|session| session.id == capture.session_id);
            if held || !owner_live {
                signal.delivery = None;
            } else if signal.delivery.is_none() {
                signal.delivery = Some(assign(signal.generation, capture.session_id.clone(), now));
            }
            continue;
        }
        if let Some(delivery) = &signal.delivery {
            if let Some(holder) = live
                .iter()
                .find(|session| session.id == delivery.session_id)
            {
                if delivery.lease_active(now) {
                    continue;
                }
                if !held {
                    if !responsive(holder) {
                        let others: Vec<&Session> = live
                            .iter()
                            .copied()
                            .filter(|session| session.id != holder.id && responsive(session))
                            .collect();
                        if let Some(session_id) = router.route(signal, &others) {
                            signal.delivery = Some(assign(signal.generation, session_id, now));
                        }
                    }
                    continue;
                }
            }
        }
        signal.delivery = if held {
            None
        } else {
            router
                .route(signal, &live)
                .map(|session_id| assign(signal.generation, session_id, now))
        };
    }
}

fn assign(generation: u64, session_id: String, now: OffsetDateTime) -> Delivery {
    Delivery {
        generation,
        session_id,
        assigned_at: now,
        lease: None,
        acked_at: None,
        wakes: 0,
    }
}

/// A report about an existing signal may not come from a weaker check of its
/// origin than the signal's own: an unverified webhook cannot resolve, refresh,
/// or re-fire a verified or locally observed alert.
fn refuse_weaker(signal: &Signal, offered: SignatureStatus) -> Result<(), SignalsError> {
    let stored = signal.provenance.signature;
    if offered.strength() < stored.strength() {
        return Err(SignalsError::WeakerSignature {
            key: signal.key.clone(),
            stored: stored.as_str(),
            offered: offered.as_str(),
        });
    }
    Ok(())
}

/// Lease this session's pending reminders in `scope` that have no lease out,
/// most severe and oldest first, up to `limit`. A lapsed lease is replaced
/// with a fresh one and the next attempt number. Returns the leased reminders
/// and how many routed to the session were left waiting.
fn lease(
    state: &mut InboxSnapshot,
    session_id: &str,
    scope: CheckInScope,
    limit: Option<usize>,
    lease_for: time::Duration,
    now: OffsetDateTime,
) -> (Vec<Reminder>, usize) {
    let mut ready: Vec<&mut Signal> = state
        .signals
        .values_mut()
        .filter(|signal| {
            signal.is_pending()
                && signal.delivery.as_ref().is_some_and(|delivery| {
                    delivery.session_id == session_id && !delivery.lease_active(now)
                })
        })
        .collect();
    ready.sort_by(|left, right| {
        right
            .severity
            .cmp(&left.severity)
            .then(left.capture_order.cmp(&right.capture_order))
            .then(left.changed_at.cmp(&right.changed_at))
            .then(left.key.cmp(&right.key))
    });
    let limit = limit.unwrap_or(usize::MAX);
    let mut reminders = Vec::new();
    let mut waiting = 0;
    for signal in ready {
        if !scope.admits(signal.severity) || reminders.len() >= limit {
            waiting += 1;
            continue;
        }
        if signal.state == SignalState::Fired {
            signal.fired_recipient = Some(session_id.to_string());
        }
        let lease = Lease {
            token: uuid::Uuid::new_v4().simple().to_string(),
            leased_at: now,
            expires_at: now + lease_for,
        };
        signal.attempts += 1;
        reminders.push(Reminder::leased(signal, &lease));
        if let Some(delivery) = signal.delivery.as_mut() {
            delivery.lease = Some(lease);
        }
    }
    (reminders, waiting)
}

/// Make room for one more signal under `max_signals`: prune the oldest
/// settled signal, or refuse when every kept signal is still fired.
fn make_room(state: &mut InboxSnapshot, max_signals: usize) -> Result<(), SignalsError> {
    while state.signals.len() >= max_signals {
        let oldest = state
            .signals
            .values()
            .filter(|signal| {
                signal.state != SignalState::Fired
                    || (signal.capture.is_some() && !signal.is_pending())
            })
            .min_by(|left, right| {
                left.changed_at
                    .cmp(&right.changed_at)
                    .then(left.key.cmp(&right.key))
            })
            .map(|signal| signal.key.clone());
        match oldest {
            Some(key) => {
                state.signals.remove(&key);
            }
            None => return Err(SignalsError::Full(state.signals.len())),
        }
    }
    Ok(())
}

fn read_document(path: &Path) -> Result<InboxSnapshot, SignalsError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(InboxSnapshot::default()),
        Err(source) => return Err(SignalsError::io(path, source)),
    };
    let document: Document =
        serde_json::from_slice(&bytes).map_err(|source| SignalsError::Corrupt {
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;
    if document.schema != INBOX_SCHEMA {
        return Err(SignalsError::Corrupt {
            path: path.to_path_buf(),
            message: format!("schema `{}` is not `{INBOX_SCHEMA}`", document.schema),
        });
    }
    Ok(document.state)
}

/// Write beside the target and rename over it, so a reader never sees a
/// partial document.
fn write_document(path: &Path, state: InboxSnapshot) -> Result<(), SignalsError> {
    let document = Document {
        schema: INBOX_SCHEMA.to_string(),
        state,
    };
    let bytes = serde_json::to_vec_pretty(&document).map_err(|source| SignalsError::Corrupt {
        path: path.to_path_buf(),
        message: source.to_string(),
    })?;
    let temp = path.with_file_name(format!(
        ".{INBOX_FILE}.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let written = (|| {
        let mut file = open_private(&temp)?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|source| SignalsError::io(&temp, source))?;
        fs::rename(&temp, path).map_err(|source| SignalsError::io(path, source))
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// Remove what a killed process left: a temporary inbox file whose rename
/// never ran, and a watcher lock no watcher holds. Best effort; debris is
/// harmless, only untidy.
fn sweep_debris(dir: &Path, sessions: &[String]) {
    let prefix = format!(".{INBOX_FILE}.");
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&prefix) && name.ends_with(".tmp") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    watcher::sweep_orphans(dir, sessions);
}

pub(crate) fn open_private(path: &Path) -> Result<File, SignalsError> {
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|source| SignalsError::io(path, source))
}

pub(crate) fn create_private_dir(dir: &Path) -> Result<(), SignalsError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|source| SignalsError::io(dir, source))
}
