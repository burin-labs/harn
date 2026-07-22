use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use super::sinks::flush_all_sinks;
use super::{AgentEvent, AgentEventSink, AgentEventSinkError};

#[cfg(test)]
#[derive(Clone)]
pub(super) struct RegisteredSink {
    pub(super) owner: std::thread::ThreadId,
    pub(super) sink: Arc<dyn AgentEventSink>,
}

#[cfg(not(test))]
pub(super) type RegisteredSink = Arc<dyn AgentEventSink>;

type ExternalSinkRegistry = RwLock<HashMap<String, Vec<RegisteredSink>>>;

fn external_sinks() -> &'static ExternalSinkRegistry {
    static REGISTRY: OnceLock<ExternalSinkRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn register_sink(session_id: impl Into<String>, sink: Arc<dyn AgentEventSink>) {
    let session_id = session_id.into();
    let mut reg = external_sinks().write().expect("sink registry poisoned");
    #[cfg(test)]
    let sink = RegisteredSink {
        owner: std::thread::current().id(),
        sink,
    };
    reg.entry(session_id).or_default().push(sink);
}

/// Remove all external sinks registered for `session_id`. Does NOT
/// close the session itself — subscribers and transcript survive, so a
/// later `agent_loop` call with the same id continues the conversation.
pub fn clear_session_sinks(session_id: &str) {
    #[cfg(test)]
    {
        external_sinks()
            .write()
            .expect("sink registry poisoned")
            .remove(session_id);
    }
    #[cfg(not(test))]
    {
        external_sinks()
            .write()
            .expect("sink registry poisoned")
            .remove(session_id);
    }
}

pub fn reset_all_sinks() {
    #[cfg(test)]
    {
        let owner = std::thread::current().id();
        let mut reg = external_sinks().write().expect("sink registry poisoned");
        reg.retain(|_, sinks| {
            sinks.retain(|sink| sink.owner != owner);
            !sinks.is_empty()
        });
        crate::agent_sessions::reset_session_store();
        reset_wildcard_sinks();
    }
    #[cfg(not(test))]
    {
        external_sinks()
            .write()
            .expect("sink registry poisoned")
            .clear();
        crate::agent_sessions::reset_session_store();
        wildcard_sinks()
            .write()
            .expect("wildcard registry poisoned")
            .clear();
    }
}

/// Mirror externally-registered sinks from `source_session_id` onto
/// `target_session_id` without moving ownership. Transports such as ACP
/// register sinks on the outer prompt session before a script runs; scripts
/// may then open a first-class agent transcript and route `agent_loop` events
/// through that inner id. Mirroring keeps the transport subscribed to the
/// in-run child transcript while preserving explicit session ids.
pub fn mirror_session_sinks(source_session_id: &str, target_session_id: &str) {
    if source_session_id.is_empty() || target_session_id.is_empty() {
        return;
    }
    if source_session_id == target_session_id {
        return;
    }
    let mut reg = external_sinks().write().expect("sink registry poisoned");
    let Some(source_sinks) = reg.get(source_session_id).cloned() else {
        return;
    };
    let target = reg.entry(target_session_id.to_string()).or_default();
    #[cfg(test)]
    {
        for source in source_sinks {
            let already_present = target
                .iter()
                .any(|existing| Arc::ptr_eq(&existing.sink, &source.sink));
            if !already_present {
                target.push(source);
            }
        }
    }
    #[cfg(not(test))]
    {
        for source in source_sinks {
            let already_present = target.iter().any(|existing| Arc::ptr_eq(existing, &source));
            if !already_present {
                target.push(source);
            }
        }
    }
}

/// Emit an event to external sinks registered for this session. Pipeline
/// closure subscribers are NOT called by this function — the agent
/// loop owns that path because it needs its async VM context.
///
/// Wildcard sinks registered via [`register_wildcard_sink`] also receive
/// the event regardless of `session_id`. Wildcard delivery is intended
/// for cross-session observers (e.g. the DAP debugger watching every
/// subagent lifecycle in a session-agnostic process) and runs after the
/// session-scoped fan-out so per-session sinks always see the event
/// first when ordering matters.
pub fn emit_event(event: &AgentEvent) {
    for sink in session_sink_snapshot(event.session_id()) {
        sink.handle_event(event);
    }
    for sink in wildcard_sink_snapshot() {
        sink.handle_event(event);
    }
}

/// Establish a causal persistence barrier for every session-scoped and
/// wildcard sink that would receive an event for `session_id`. Events emitted
/// after the registry snapshots are outside the barrier; every event accepted
/// before them is awaited without polling.
pub async fn flush_session_sinks(session_id: &str) -> Result<(), AgentEventSinkError> {
    let mut sinks = session_sink_snapshot(session_id);
    sinks.extend(wildcard_sink_snapshot());
    flush_all_sinks(sinks).await
}

/// Flush every sink that can observe `session_id`, then remove the
/// session-scoped registrations. The removal still happens when persistence
/// fails so a completed transport cannot leak live sinks into a later turn.
pub async fn flush_and_clear_session_sinks(session_id: &str) -> Result<(), AgentEventSinkError> {
    let result = flush_session_sinks(session_id).await;
    clear_session_sinks(session_id);
    result
}

/// Opaque handle returned by [`register_wildcard_sink`]. Pass back to
/// [`unregister_wildcard_sink`] to drop the registration without
/// disturbing other wildcard observers. Cloneable so a sink owner can
/// stash the handle alongside the sink itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct WildcardSinkHandle(pub(crate) u64);

#[cfg(test)]
#[derive(Clone)]
struct WildcardSinkEntry {
    handle: WildcardSinkHandle,
    owner: std::thread::ThreadId,
    sink: Arc<dyn AgentEventSink>,
}

#[cfg(not(test))]
#[derive(Clone)]
struct WildcardSinkEntry {
    handle: WildcardSinkHandle,
    sink: Arc<dyn AgentEventSink>,
}

type WildcardSinkRegistry = RwLock<Vec<WildcardSinkEntry>>;

fn wildcard_sinks() -> &'static WildcardSinkRegistry {
    static REGISTRY: OnceLock<WildcardSinkRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

fn session_sink_snapshot(session_id: &str) -> Vec<Arc<dyn AgentEventSink>> {
    let reg = external_sinks().read().expect("sink registry poisoned");
    #[cfg(test)]
    {
        reg.get(session_id)
            .map(|sinks| sinks.iter().map(|sink| sink.sink.clone()).collect())
            .unwrap_or_default()
    }
    #[cfg(not(test))]
    {
        reg.get(session_id).cloned().unwrap_or_default()
    }
}

fn wildcard_sink_snapshot() -> Vec<Arc<dyn AgentEventSink>> {
    let reg = wildcard_sinks().read().expect("wildcard registry poisoned");
    #[cfg(test)]
    {
        let owner = std::thread::current().id();
        reg.iter()
            .filter(|entry| entry.owner == owner)
            .map(|entry| entry.sink.clone())
            .collect()
    }
    #[cfg(not(test))]
    {
        reg.iter().map(|entry| entry.sink.clone()).collect()
    }
}

pub(super) fn session_has_live_subscriber(session_id: &str) -> bool {
    session_sink_snapshot(session_id)
        .into_iter()
        .chain(wildcard_sink_snapshot())
        .any(|sink| !sink.is_persistence_sink())
        || crate::agent_sessions::subscriber_count(session_id) > 0
}

fn next_wildcard_handle() -> WildcardSinkHandle {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    WildcardSinkHandle(COUNTER.fetch_add(1, Ordering::SeqCst))
}

/// Register a sink that receives **every** emitted `AgentEvent`
/// regardless of `session_id`. Intended for cross-session observers
/// such as the DAP debugger, which surfaces every subagent's lifecycle
/// without knowing the session id ahead of time.
///
/// Returns a [`WildcardSinkHandle`] the caller passes to
/// [`unregister_wildcard_sink`] when shutting down. Without that pairing
/// the sink leaks for the lifetime of the process — caller-managed
/// drop is the contract because the registry is process-global.
pub fn register_wildcard_sink(sink: Arc<dyn AgentEventSink>) -> WildcardSinkHandle {
    let handle = next_wildcard_handle();
    let mut reg = wildcard_sinks()
        .write()
        .expect("wildcard registry poisoned");
    #[cfg(test)]
    let entry = WildcardSinkEntry {
        handle,
        owner: std::thread::current().id(),
        sink,
    };
    #[cfg(not(test))]
    let entry = WildcardSinkEntry { handle, sink };
    reg.push(entry);
    handle
}

/// Drop the wildcard sink registered under `handle`. Idempotent — no-op
/// when the handle is unknown (already-unregistered or never-issued).
pub fn unregister_wildcard_sink(handle: WildcardSinkHandle) {
    let mut reg = wildcard_sinks()
        .write()
        .expect("wildcard registry poisoned");
    reg.retain(|entry| entry.handle != handle);
}

/// Test-only: clear every wildcard sink. Mirrors
/// [`reset_all_sinks`] for the per-session registry so test setups can
/// guarantee a clean baseline without retaining stray handles.
#[cfg(test)]
pub fn reset_wildcard_sinks() {
    let owner = std::thread::current().id();
    let mut reg = wildcard_sinks()
        .write()
        .expect("wildcard registry poisoned");
    reg.retain(|entry| entry.owner != owner);
}

pub fn session_external_sink_count(session_id: &str) -> usize {
    #[cfg(test)]
    {
        return external_sinks()
            .read()
            .expect("sink registry poisoned")
            .get(session_id)
            .map(Vec::len)
            .unwrap_or(0);
    }
    #[cfg(not(test))]
    {
        external_sinks()
            .read()
            .expect("sink registry poisoned")
            .get(session_id)
            .map(|v| v.len())
            .unwrap_or(0)
    }
}

/// Return whether `sink` is still registered for `session_id`.
///
/// Request-local transports use this to install a scoped fallback sink without
/// double-delivering events while their process-global registration is healthy.
/// If sibling reset code clears the global registration mid-dispatch, the
/// scoped sink can detect that absence and continue streaming live events.
pub fn session_has_external_sink(session_id: &str, sink: &Arc<dyn AgentEventSink>) -> bool {
    let reg = external_sinks().read().expect("sink registry poisoned");
    #[cfg(test)]
    {
        reg.get(session_id)
            .is_some_and(|sinks| sinks.iter().any(|entry| Arc::ptr_eq(&entry.sink, sink)))
    }
    #[cfg(not(test))]
    {
        reg.get(session_id)
            .is_some_and(|sinks| sinks.iter().any(|entry| Arc::ptr_eq(entry, sink)))
    }
}

pub fn session_closure_subscriber_count(session_id: &str) -> usize {
    crate::agent_sessions::subscriber_count(session_id)
}
