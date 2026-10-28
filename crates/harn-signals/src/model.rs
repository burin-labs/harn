//! Signals, sessions, and the reminders leased to sessions.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::event::TriageEvent;
use crate::SignalsError;

/// Where a signal is in its lifecycle. Each transition is one generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalState {
    Fired,
    Resolved,
    Expired,
}

impl SignalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fired => "fired",
            Self::Resolved => "resolved",
            Self::Expired => "expired",
        }
    }
}

/// How much a signal deserves the session's attention. Only `urgent` may keep
/// a stopping session going or wake an idle one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Urgent,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Urgent => "urgent",
        }
    }
}

impl FromStr for Severity {
    type Err = SignalsError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "info" => Ok(Self::Info),
            "warning" => Ok(Self::Warning),
            "urgent" => Ok(Self::Urgent),
            other => Err(SignalsError::InvalidInput(format!(
                "severity must be info, warning, or urgent, not `{other}`"
            ))),
        }
    }
}

/// Which pending reminders a check-in leases.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckInScope {
    /// Every pending reminder routed to the session.
    All,
    /// Only urgent reminders: the session is stopping or idle, and anything
    /// less waits for its next turn.
    UrgentOnly,
}

impl CheckInScope {
    pub(crate) fn admits(self, severity: Severity) -> bool {
        match self {
            Self::All => true,
            Self::UrgentOnly => severity == Severity::Urgent,
        }
    }
}

/// Whether the producer's claim about where an alert came from was checked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureStatus {
    /// A webhook whose signature the receiving connector verified.
    Verified,
    /// Arrived over the network without a verified signature.
    Unverified,
    /// Observed on this machine, for example by a polling monitor or by hand;
    /// no signature applies.
    #[default]
    Local,
}

impl SignatureStatus {
    /// How much the origin claim was checked. An unverified network
    /// delivery may not override a verified or local one.
    pub fn strength(self) -> u8 {
        match self {
            Self::Unverified => 0,
            Self::Verified | Self::Local => 1,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Unverified => "unverified",
            Self::Local => "local",
        }
    }
}

/// Who fired a signal and how far its origin was checked. Shown beside every
/// reminder so the reader can weigh it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The producer, such as `std/signals.monitor_github_ci` or `harn signals fire`.
    pub producer: String,
    #[serde(default)]
    pub signature: SignatureStatus,
}

impl Provenance {
    pub fn local(producer: impl Into<String>) -> Self {
        Self {
            producer: producer.into(),
            signature: SignatureStatus::Local,
        }
    }
}

/// Hints the router matches against registered sessions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingHints {
    /// `owner/name`, compared case-insensitively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// A directory; a session whose working directory is inside it matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Free-form topic for routers that understand it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

/// What a producer hands the inbox to fire a signal.
#[derive(Clone, Debug)]
pub struct SignalInput {
    /// The condition's identity. Defaults to the event's `dedupe_key`.
    pub key: Option<String>,
    pub event: TriageEvent,
    pub severity: Severity,
    pub routing: RoutingHints,
    pub provenance: Provenance,
    /// When an unresolved signal lapses.
    pub expires_at: Option<OffsetDateTime>,
}

/// One outstanding hand-off of a reminder to its session. Only the holder of
/// `token`, the check-in that leased it, can ack it; a lease that lapses
/// without that ack is leased again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Lease {
    pub token: String,
    #[serde(with = "time::serde::rfc3339")]
    pub leased_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

/// Stable identity of one captured monitor occurrence, scoped to its session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureIdentity {
    pub session_id: String,
    pub monitor_key: String,
    pub event_id: String,
}

/// The current generation's delivery: the session it is routed to, any
/// outstanding lease, and the ack that settles it.
/// Only the inbox creates one, so it can gain fields without breaking readers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Delivery {
    pub generation: u64,
    pub session_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub assigned_at: OffsetDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<Lease>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub acked_at: Option<OffsetDateTime>,
    /// How many times an idle watcher woke the session for this delivery.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub wakes: u32,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl Delivery {
    pub fn lease_active(&self, now: OffsetDateTime) -> bool {
        self.lease
            .as_ref()
            .is_some_and(|lease| lease.expires_at > now)
    }
}

/// One condition and its lifecycle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    /// Present for immutable occurrences; these never route to another session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<CaptureIdentity>,
    /// Ingestion order among retained captures, independent of wall-clock ties.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_order: Option<u64>,
    pub key: String,
    pub state: SignalState,
    pub generation: u64,
    pub severity: Severity,
    pub event: TriageEvent,
    #[serde(default)]
    pub routing: RoutingHints,
    pub provenance: Provenance,
    #[serde(with = "time::serde::rfc3339")]
    pub fired_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub changed_at: OffsetDateTime,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    /// Delivery of the current generation, once routed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    /// Leases granted for the current generation, counting redeliveries to
    /// any session.
    #[serde(default)]
    pub attempts: u32,
    /// The session leased the latest `fired` generation. Its follow-up
    /// (resolved or expired) is owed to the same session, and is suppressed
    /// when no session was ever leased the fired reminder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_recipient: Option<String>,
}

impl Signal {
    /// Whether the current generation still owes an acked delivery.
    pub fn is_pending(&self) -> bool {
        if self
            .delivery
            .as_ref()
            .is_some_and(|delivery| delivery.acked_at.is_some())
        {
            return false;
        }
        match self.state {
            SignalState::Fired => true,
            SignalState::Resolved | SignalState::Expired => self.fired_recipient.is_some(),
        }
    }

    pub fn id(&self) -> ReminderId {
        ReminderId {
            key: self.key.clone(),
            generation: self.generation,
        }
    }
}

/// The agent harness a session runs in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", from = "String")]
pub enum HarnessKind {
    ClaudeCode,
    Codex,
    Other(String),
}

impl fmt::Display for HarnessKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClaudeCode => formatter.write_str("claude-code"),
            Self::Codex => formatter.write_str("codex"),
            Self::Other(name) => formatter.write_str(name),
        }
    }
}

impl From<String> for HarnessKind {
    fn from(value: String) -> Self {
        match value.as_str() {
            "claude-code" => Self::ClaudeCode,
            "codex" => Self::Codex,
            _ => Self::Other(value),
        }
    }
}

impl From<HarnessKind> for String {
    fn from(value: HarnessKind) -> Self {
        value.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Busy,
    Idle,
}

/// What a session says about itself each time it checks in.
#[derive(Clone, Debug, Deserialize)]
pub struct SessionAnnouncement {
    pub id: String,
    pub harness: HarnessKind,
    pub cwd: PathBuf,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// A role the router knows, such as the fallback role `control`.
    #[serde(default)]
    pub role: Option<String>,
    pub activity: Activity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub harness: HarnessKind,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub activity: Activity,
    #[serde(with = "time::serde::rfc3339")]
    pub registered_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: OffsetDateTime,
}

/// Names one generation of one signal. Rendering is keyed by it, so a
/// redelivered reminder renders identically and a consumer can dedupe on it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ReminderId {
    pub key: String,
    pub generation: u64,
}

impl fmt::Display for ReminderId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}#{}", self.key, self.generation)
    }
}

/// One generation of one signal, leased to one session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reminder {
    pub key: String,
    pub generation: u64,
    pub state: SignalState,
    pub severity: Severity,
    pub event: TriageEvent,
    pub routing: RoutingHints,
    pub provenance: Provenance,
    /// 1 on first delivery; higher when a lapsed lease is redelivered.
    pub attempt: u32,
    /// Ack with this once the reminder has been handed to the model.
    pub lease_token: String,
    #[serde(with = "time::serde::rfc3339")]
    pub lease_expires_at: OffsetDateTime,
}

impl Reminder {
    pub fn id(&self) -> ReminderId {
        ReminderId {
            key: self.key.clone(),
            generation: self.generation,
        }
    }

    pub(crate) fn leased(signal: &Signal, lease: &Lease) -> Self {
        Self {
            key: signal.key.clone(),
            generation: signal.generation,
            state: signal.state,
            severity: signal.severity,
            event: signal.event.clone(),
            routing: signal.routing.clone(),
            provenance: signal.provenance.clone(),
            attempt: signal.attempts,
            lease_token: lease.token.clone(),
            lease_expires_at: lease.expires_at,
        }
    }
}

/// What one check-in did.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CheckIn {
    /// Reminders leased now. Hand them to the model, then ack their
    /// `lease_token`s; an unacked lease is leased again when it lapses.
    pub reminders: Vec<Reminder>,
    /// Reminders routed to this session that this check-in did not lease,
    /// because of its scope or limit.
    pub waiting: usize,
}
