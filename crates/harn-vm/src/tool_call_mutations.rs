//! Completed workspace mutations, by session and then by the tool call that
//! made them.
//!
//! A call the loop stops before it returns has no result of its own. This
//! record is what lets it still report that it changed the workspace
//! (harn#9611). It depends on nothing above it, so the agent loop can read it
//! without reaching up into session state; recording against the *current*
//! call lives in `agent_sessions`, which knows which call that is.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

type Store = BTreeMap<String, BTreeMap<String, BTreeSet<String>>>;

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Record that `tool_call_id` in `session_id` finished mutating `path`. Call
/// it only after the mutation succeeded; a no-op when any argument is empty.
pub(crate) fn record(session_id: &str, tool_call_id: &str, path: &str) {
    if session_id.is_empty() || tool_call_id.is_empty() || path.is_empty() {
        return;
    }
    if let Ok(mut store) = store().lock() {
        store
            .entry(session_id.to_string())
            .or_default()
            .entry(tool_call_id.to_string())
            .or_default()
            .insert(path.to_string());
    }
}

/// Read and release the paths the call finished mutating, sorted; empty when
/// it recorded none.
pub(crate) fn take(session_id: &str, tool_call_id: &str) -> Vec<String> {
    let Ok(mut store) = store().lock() else {
        return Vec::new();
    };
    let Some(calls) = store.get_mut(session_id) else {
        return Vec::new();
    };
    let paths = calls.remove(tool_call_id).unwrap_or_default();
    if calls.is_empty() {
        store.remove(session_id);
    }
    paths.into_iter().collect()
}

/// Drop every call record a session still holds.
pub(crate) fn clear_session(session_id: &str) {
    if let Ok(mut store) = store().lock() {
        store.remove(session_id);
    }
}

/// Drop every session's call records at process teardown.
pub(crate) fn clear_all() {
    if let Ok(mut store) = store().lock() {
        store.clear();
    }
}
