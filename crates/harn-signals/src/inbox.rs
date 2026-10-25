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
    Activity, CheckIn, CheckInScope, Delivery, Lease, Reminder, ReminderId, Session,
    SessionAnnouncement, Signal, SignalInput, SignalState,
};
use crate::router::{Router, RuleRouter};
use crate::SignalsError;

pub const INBOX_SCHEMA: &str = "harn.signals.inbox.v1";
pub const SIGNALS_DIR_ENV: &str = "HARN_EXT_SIGNALS_DIR";

const INBOX_FILE: &str = "inbox.json";
const LOCK_FILE: &str = "inbox.lock";
/// Transactions are milliseconds; a wait this long means a wedged holder.
const LOCK_DEADLINE: Duration = Duration::from_secs(10);
/// A check-in inside this window only refreshes `last_seen` in memory, so a
/// burst of tool calls does not rewrite the file on every hook.
const HEARTBEAT_GRANULARITY: time::Duration = time::Duration::seconds(10);

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
#[derive(Clone, Debug, PartialEq)]
pub enum IdlePoll {
    /// Urgent reminders leased to this session; possibly none yet.
    Reminders(Vec<Reminder>),
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

    /// Fire a signal. Firing one that is already fired refreshes its content
    /// and owes no reminder.
    pub fn fire(
        &self,
        input: SignalInput,
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
        event.validate()?;
        event.strip_raw_payload();
        let key = key.unwrap_or_else(|| event.dedupe_key.clone());
        if key.trim().is_empty() {
            return Err(SignalsError::InvalidInput("signal key is empty".into()));
        }
        if provenance.producer.trim().is_empty() {
            return Err(SignalsError::InvalidInput(
                "signal provenance names no producer".into(),
            ));
        }
        self.transact(now, |state| {
            let signal = match state.signals.get_mut(&key) {
                Some(signal) => signal,
                None => {
                    state.signals.insert(
                        key.clone(),
                        Signal {
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
                    return Transition::Changed {
                        state: SignalState::Fired,
                        generation: 1,
                    };
                }
            };
            signal.event = event;
            signal.severity = severity;
            signal.routing = routing;
            signal.provenance = provenance;
            signal.expires_at = expires_at;
            if signal.state == SignalState::Fired {
                return Transition::Unchanged {
                    state: SignalState::Fired,
                    generation: signal.generation,
                };
            }
            signal.fired_recipient = None;
            signal.fired_at = now;
            advance(signal, SignalState::Fired, now)
        })
    }

    /// Resolve a fired signal, optionally replacing its content with the
    /// recovery event. Resolving anything else changes nothing.
    pub fn resolve(
        &self,
        key: &str,
        event: Option<TriageEvent>,
        now: OffsetDateTime,
    ) -> Result<Transition, SignalsError> {
        let event = match event {
            Some(mut event) => {
                event.validate()?;
                event.strip_raw_payload();
                Some(event)
            }
            None => None,
        };
        self.transact(now, |state| {
            let Some(signal) = state.signals.get_mut(key) else {
                return Transition::Unknown;
            };
            if signal.state != SignalState::Fired {
                return Transition::Unchanged {
                    state: signal.state,
                    generation: signal.generation,
                };
            }
            if let Some(event) = event {
                signal.event = event;
            }
            advance(signal, SignalState::Resolved, now)
        })
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

    /// Check in: ack the leases this session still holds from earlier
    /// check-ins, then lease up to `limit` pending reminders in `scope`.
    ///
    /// Delivery is at least once. A leased reminder is acked by the session's
    /// next check-in, or explicitly with [`Inbox::ack`]; one whose lease lapses
    /// first is leased again, to this session if it is still live or to
    /// another one. Callers render each leased reminder by its
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
            let acked = ack_held(state, &session_id, now);
            route_pending(state, router, config, now);
            let (reminders, waiting) = lease(state, &session_id, scope, limit, lease_for, now);
            CheckIn {
                acked,
                reminders,
                waiting,
            }
        })
    }

    /// One poll of an idle session's background watcher: refresh its
    /// heartbeat and lease urgent reminders, without overwriting the activity
    /// a foreground hook recorded since the watcher started.
    pub fn poll_idle(
        &self,
        session_id: &str,
        limit: Option<usize>,
        now: OffsetDateTime,
    ) -> Result<IdlePoll, SignalsError> {
        let lease_for = self.config.lease_for;
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
            let (reminders, _) = lease(
                state,
                session_id,
                CheckInScope::UrgentOnly,
                limit,
                lease_for,
                now,
            );
            IdlePoll::Reminders(reminders)
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
            write_document(&path, state)?;
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

/// Expire lapsed signals, prune settled ones past retention, drop stale
/// sessions, and release lapsed leases so they are leased again.
fn maintain(state: &mut InboxSnapshot, config: &Config, now: OffsetDateTime) {
    for signal in state.signals.values_mut() {
        if signal.state == SignalState::Fired
            && signal
                .expires_at
                .is_some_and(|expires_at| expires_at <= now)
        {
            advance(signal, SignalState::Expired, now);
        }
        if let Some(delivery) = signal.delivery.as_mut() {
            if delivery.acked_at.is_none()
                && delivery.lease.is_some()
                && !delivery.lease_active(now)
            {
                delivery.lease = None;
            }
        }
    }
    state.signals.retain(|_, signal| {
        signal.state == SignalState::Fired || now - signal.changed_at < config.retain_for
    });
    state
        .sessions
        .retain(|_, session| now - session.last_seen <= config.stale_after);
}

/// Give every pending generation a live recipient the policy allows, keeping
/// existing live assignments so a reminder does not move once routed. An
/// unleased assignment the policy now holds is withdrawn; an outstanding lease
/// is left to be acked.
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
    for signal in state.signals.values_mut() {
        if !signal.is_pending() {
            continue;
        }
        let leased = signal
            .delivery
            .as_ref()
            .is_some_and(|delivery| delivery.lease_active(now));
        let held = config.policy.hold(signal, now).is_some();
        if let Some(delivery) = &signal.delivery {
            let recipient_live = live.iter().any(|session| session.id == delivery.session_id);
            // A lease whose holder went away is rerouted now rather than
            // waiting out a lease nobody can ack.
            if recipient_live && (leased || !held) {
                continue;
            }
        }
        signal.delivery = if held {
            None
        } else {
            router.route(signal, &live).map(|session_id| Delivery {
                generation: signal.generation,
                session_id,
                assigned_at: now,
                lease: None,
                acked_at: None,
            })
        };
    }
}

/// Ack every lease this session holds from an earlier transaction: checking
/// in again is the evidence its last output reached the model.
fn ack_held(state: &mut InboxSnapshot, session_id: &str, now: OffsetDateTime) -> Vec<ReminderId> {
    let mut acked = Vec::new();
    for signal in state.signals.values_mut() {
        let id = signal.id();
        let Some(delivery) = signal.delivery.as_mut() else {
            continue;
        };
        if delivery.session_id == session_id
            && delivery.acked_at.is_none()
            && delivery.lease_active(now)
        {
            delivery.acked_at = Some(now);
            delivery.lease = None;
            acked.push(id);
        }
    }
    acked
}

/// Lease this session's pending, unleased reminders in `scope`, most severe
/// and oldest first, up to `limit`. Returns the leased reminders and how many
/// routed to the session were left waiting.
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
            leased_at: now,
            expires_at: now + lease_for,
        };
        let Some(delivery) = signal.delivery.as_mut() else {
            continue;
        };
        delivery.lease = Some(lease);
        signal.attempts += 1;
        reminders.push(Reminder::leased(signal, lease));
    }
    (reminders, waiting)
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

fn open_private(path: &Path) -> Result<File, SignalsError> {
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

fn create_private_dir(dir: &Path) -> Result<(), SignalsError> {
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
