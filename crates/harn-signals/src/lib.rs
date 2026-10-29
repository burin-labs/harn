//! Ambient signals: a per-machine inbox that turns alert conditions into
//! reminders for running agent sessions.
//!
//! A producer fires or resolves a [`Signal`] through [`Inbox`]. Sessions check
//! in through [`Inbox::check_in`], which registers them, applies the shared
//! delivery [`Policy`], routes pending signals with a [`Router`], and leases
//! each session its [`Reminder`]s. Delivery is at least once: the consumer acks
//! a lease by its token once it has handed the reminder to the model, and a
//! lease that lapses unacked is leased again.
//! Rendering a reminder into prompt text belongs to the runtime's prompt
//! engine, not to this crate.

mod config;
mod dispatch;
mod error;
mod event;
mod inbox;
mod model;
mod repo;
mod router;
mod text;
mod watcher;

pub use config::{Config, Hold, Policy, QuietHours, CONFIG_FILE};
pub use dispatch::{
    DispatchBatch, DispatchClaim, DispatchPoll, DispatchState, DEFAULT_DISPATCH_INTERVAL_SECONDS,
};
pub use error::SignalsError;
pub use event::{
    ManualEvent, TriageActionIntent, TriageActor, TriageEvent, TriagePrivacy, TriageRelatedRef,
    MAX_KEPT_BYTES, MAX_LIST_ENTRIES, MAX_SUMMARY_CHARS, TRIAGE_EVENT_SCHEMA,
};
pub use inbox::{
    IdlePoll, Inbox, InboxSnapshot, Transition, INBOX_SCHEMA, MAX_WAKES_PER_REMINDER,
    SIGNALS_DIR_ENV,
};
pub use model::{
    Activity, CaptureIdentity, CheckIn, CheckInScope, Delivery, HarnessKind, Lease, Provenance,
    Reminder, ReminderId, RoutingHints, Session, SessionAnnouncement, Severity, Signal,
    SignalInput, SignalState, SignatureStatus,
};
pub use repo::{repo_slug, slug_from_url};
pub use router::{Router, RuleRouter};
pub use text::{display_safe, is_unsafe_display_char};
pub use watcher::WatcherLock;
