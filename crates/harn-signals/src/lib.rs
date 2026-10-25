//! Ambient signals: a per-machine inbox that turns alert conditions into
//! reminders for running agent sessions.
//!
//! A producer fires or resolves a [`Signal`] through [`Inbox`]. Sessions check
//! in through [`Inbox::check_in`], which registers them, applies the shared
//! delivery [`Policy`], routes pending signals with a [`Router`], and leases
//! each session its [`Reminder`]s. Delivery is at least once: a lease is acked
//! by the session's next check-in or explicitly, and redelivered if it lapses.
//! Rendering a reminder into prompt text belongs to the runtime's prompt
//! engine, not to this crate.

mod config;
mod error;
mod event;
mod inbox;
mod model;
mod repo;
mod router;

pub use config::{Config, Hold, Policy, QuietHours, CONFIG_FILE};
pub use error::SignalsError;
pub use event::{
    ManualEvent, TriageActionIntent, TriageActor, TriageEvent, TriagePrivacy, TriageRelatedRef,
    TRIAGE_EVENT_SCHEMA,
};
pub use inbox::{IdlePoll, Inbox, InboxSnapshot, Transition, INBOX_SCHEMA, SIGNALS_DIR_ENV};
pub use model::{
    Activity, CheckIn, CheckInScope, Delivery, HarnessKind, Lease, Provenance, Reminder,
    ReminderId, RoutingHints, Session, SessionAnnouncement, Severity, Signal, SignalInput,
    SignalState, SignatureStatus,
};
pub use repo::{repo_slug, slug_from_url};
pub use router::{Router, RuleRouter};
