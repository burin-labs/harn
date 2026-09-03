use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::{SessionState, SessionTranscriptBudgetPolicy, DEFAULT_SESSION_CAP};

pub(super) struct SharedCell<T>(parking_lot::RwLock<T>);

impl<T> SharedCell<T> {
    pub(super) fn borrow(&self) -> parking_lot::RwLockReadGuard<'_, T> {
        self.0.read()
    }

    pub(super) fn borrow_mut(&self) -> parking_lot::RwLockWriteGuard<'_, T> {
        self.0.write()
    }

    pub(super) fn try_borrow(&self) -> Result<parking_lot::RwLockReadGuard<'_, T>, ()> {
        self.0.try_read().ok_or(())
    }
}

pub(super) struct SharedValue<T: Copy>(parking_lot::Mutex<T>);

impl<T: Copy> SharedValue<T> {
    pub(super) fn get(&self) -> T {
        *self.0.lock()
    }

    pub(super) fn set(&self, value: T) {
        *self.0.lock() = value;
    }
}

pub(crate) struct AgentSessionRuntime {
    sessions: SharedCell<HashMap<String, SessionState>>,
    pub(super) unknown_host_event_warnings: SharedCell<HashSet<(String, String)>>,
    session_cap: SharedValue<usize>,
    default_transcript_budget_policy: SharedCell<SessionTranscriptBudgetPolicy>,
}

impl Default for AgentSessionRuntime {
    fn default() -> Self {
        Self {
            sessions: SharedCell(parking_lot::RwLock::new(HashMap::new())),
            unknown_host_event_warnings: SharedCell(parking_lot::RwLock::new(HashSet::new())),
            session_cap: SharedValue(parking_lot::Mutex::new(DEFAULT_SESSION_CAP)),
            default_transcript_budget_policy: SharedCell(parking_lot::RwLock::new(
                SessionTranscriptBudgetPolicy::default(),
            )),
        }
    }
}

thread_local! {
    static ACTIVE_SESSION_RUNTIME: RefCell<Arc<AgentSessionRuntime>> =
        RefCell::new(fresh_session_runtime());
}

pub(crate) fn fresh_session_runtime() -> Arc<AgentSessionRuntime> {
    Arc::new(AgentSessionRuntime::default())
}

pub(crate) fn active_session_runtime() -> Arc<AgentSessionRuntime> {
    ACTIVE_SESSION_RUNTIME.with(|slot| Arc::clone(&slot.borrow()))
}

/// Record an unknown host event name in the runtime that owns the session.
/// The runtime is shared across worker-thread migration, unlike ambient
/// thread-local storage.
pub(crate) fn mark_unknown_host_event_warning(session_id: &str, event_type: &str) -> bool {
    active_session_runtime()
        .unknown_host_event_warnings
        .borrow_mut()
        .insert((session_id.to_string(), event_type.to_string()))
}

pub(super) fn clear_unknown_host_event_warnings(session_id: &str) {
    active_session_runtime()
        .unknown_host_event_warnings
        .borrow_mut()
        .retain(|(warned_session_id, _)| warned_session_id != session_id);
}

pub(crate) fn swap_active_session_runtime(
    next: Arc<AgentSessionRuntime>,
) -> Arc<AgentSessionRuntime> {
    ACTIVE_SESSION_RUNTIME.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), next))
}

pub(super) struct SessionsSlot;
pub(super) static SESSIONS: SessionsSlot = SessionsSlot;

impl SessionsSlot {
    pub(super) fn with<T>(
        &self,
        use_sessions: impl FnOnce(&SharedCell<HashMap<String, SessionState>>) -> T,
    ) -> T {
        let runtime = active_session_runtime();
        use_sessions(&runtime.sessions)
    }
}

pub(super) struct SessionCapSlot;
pub(super) static SESSION_CAP: SessionCapSlot = SessionCapSlot;

impl SessionCapSlot {
    pub(super) fn with<T>(&self, use_cap: impl FnOnce(&SharedValue<usize>) -> T) -> T {
        let runtime = active_session_runtime();
        use_cap(&runtime.session_cap)
    }
}

pub(super) struct DefaultTranscriptBudgetPolicySlot;
pub(super) static DEFAULT_TRANSCRIPT_BUDGET_POLICY: DefaultTranscriptBudgetPolicySlot =
    DefaultTranscriptBudgetPolicySlot;

impl DefaultTranscriptBudgetPolicySlot {
    pub(super) fn with<T>(
        &self,
        use_policy: impl FnOnce(&SharedCell<SessionTranscriptBudgetPolicy>) -> T,
    ) -> T {
        let runtime = active_session_runtime();
        use_policy(&runtime.default_transcript_budget_policy)
    }
}
