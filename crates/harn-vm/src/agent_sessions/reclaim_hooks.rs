//! Hooks that reclaim what a session owns for its whole life.
//!
//! Some resources outlive the agent-loop run that created them, such as a
//! background command a turn started and a later turn polls. They belong to the
//! session, so they are reclaimed when the session closes or when a stop
//! abandons its run, and not each time a run over the session finishes: a host
//! opens a session once and runs one loop per turn.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

/// Receives the id of the session whose resources must be released.
pub type SessionReclaimHook = Arc<dyn Fn(&str) + Send + Sync>;

static NEXT_HOOK_ID: AtomicU64 = AtomicU64::new(1);
static HOOKS: LazyLock<Mutex<BTreeMap<u64, SessionReclaimHook>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Owns one reclaim-hook registration; dropping it unregisters the hook.
#[must_use = "dropping the registration unregisters the session reclaim hook"]
pub struct SessionReclaimHookRegistration {
    id: u64,
}

impl Drop for SessionReclaimHookRegistration {
    fn drop(&mut self) {
        HOOKS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

/// Register a hook that runs when a session is closed or a stop abandons its
/// run.
pub fn register_session_reclaim_hook(hook: SessionReclaimHook) -> SessionReclaimHookRegistration {
    let id = NEXT_HOOK_ID.fetch_add(1, Ordering::Relaxed);
    HOOKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(id, hook);
    SessionReclaimHookRegistration { id }
}

/// Release what a closed session held: its changed-path record and every
/// resource a reclaim hook owns for it.
pub(super) fn release_closed_session(session_id: &str) {
    super::clear_session_changed_paths(session_id);
    fire(session_id);
}

pub(crate) fn fire(session_id: &str) {
    let hooks = HOOKS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for hook in hooks {
        hook(session_id);
    }
}
