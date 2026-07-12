//! Agent event stream — the ACP-aligned observation surface for the
//! agent loop.
//!
//! Every phase of the agent loop emits an `AgentEvent`. The canonical
//! variants map 1:1 onto ACP `SessionUpdate` values; three internal
//! variants (`IterationStart`, `IterationEnd`, `FeedbackInjected`) let
//! pipelines react to loop milestones that don't have a direct ACP
//! counterpart.
//!
//! `iteration_*` event names refer to one model round-trip inside the
//! loop (Harn's "iteration"), not to ACP's outer `prompt_turn`. See
//! `docs/src/concepts/glossary.md` for the canonical vocabulary and
//! `docs/src/concepts/sota-comparison.md` for the ACP mapping.
//!
//! There are two subscription paths, both keyed on session id so two
//! concurrent sessions never cross-talk:
//!
//! 1. **External sinks** (`AgentEventSink` trait) — Rust-side consumers
//!    like the harn-cli ACP server. Invoked synchronously by the loop.
//!    Stored in a global `OnceLock<RwLock<HashMap<...>>>` here.
//! 2. **Closure subscribers** — `.harn` closures registered via the
//!    `agent_subscribe(session_id, callback)` host builtin. These live
//!    on the session's `SessionState.subscribers` in
//!    `crate::agent_sessions`, because sessions are the single source
//!    of truth for session-scoped VM state.

mod agent;
mod from_host;
mod host_injection;
mod registry;
mod sinks;
mod tool;
mod worker;

#[cfg(test)]
mod tests;

pub use agent::AgentEvent;
pub use host_injection::{
    AttachmentFlavor, AttachmentRendering, HostInjectionProvenance, InjectionDelivery,
    SanitizationAction, SanitizationVerdict,
};
#[cfg(test)]
pub use registry::reset_wildcard_sinks;
pub use registry::{
    clear_session_sinks, emit_event, mirror_session_sinks, register_sink, register_wildcard_sink,
    reset_all_sinks, session_closure_subscriber_count, session_external_sink_count,
    session_has_external_sink, unregister_wildcard_sink, WildcardSinkHandle,
};
pub use sinks::{AgentEventSink, EventLogSink, JsonlEventSink, MultiSink, PersistedAgentEvent};
pub use tool::{
    DenialGate, ToolCallErrorCategory, ToolCallStatus, ToolDenial, ToolExecutor, ToolMutationStatus,
};
pub use worker::{FsWatchEvent, WorkerEvent};
