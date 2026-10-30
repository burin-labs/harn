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

    pub fn set(&self, value: T) {
        *self.0.lock() = value;
    }
}

pub struct AgentSessionRuntime {
    sessions: SharedCell<HashMap<String, SessionState>>,
    inputs: parking_lot::RwLock<
        HashMap<
            String,
            Arc<parking_lot::Mutex<harn_execution_policy::session_inputs::SessionInputs>>,
        >,
    >,
    pub(super) unknown_host_event_warnings: SharedCell<HashSet<(String, String)>>,
    /// One entry per (session, event type, dropped key) already reported.
    pub(super) dropped_host_payload_key_warnings: SharedCell<HashSet<(String, String, String)>>,
    session_cap: SharedValue<usize>,
    default_transcript_budget_policy: SharedCell<SessionTranscriptBudgetPolicy>,
}

impl Default for AgentSessionRuntime {
    fn default() -> Self {
        Self {
            sessions: SharedCell(parking_lot::RwLock::new(HashMap::new())),
            inputs: parking_lot::RwLock::new(HashMap::new()),
            unknown_host_event_warnings: SharedCell(parking_lot::RwLock::new(HashSet::new())),
            dropped_host_payload_key_warnings: SharedCell(parking_lot::RwLock::new(HashSet::new())),
            session_cap: SharedValue(parking_lot::Mutex::new(DEFAULT_SESSION_CAP)),
            default_transcript_budget_policy: SharedCell(parking_lot::RwLock::new(
                SessionTranscriptBudgetPolicy::default(),
            )),
        }
    }
}

impl AgentSessionRuntime {
    pub(crate) fn input_store(
        &self,
        id: &str,
        create: bool,
    ) -> Result<crate::session_inputs::Store, crate::session_inputs::InputError> {
        // Retirement detaches existing bindings. Filtering may read them while
        // a transcript mutation holds the session lock, so this path stays independent.
        if let Some(store) = self.inputs.read().get(id).cloned() {
            return Ok(store);
        }
        if !create {
            return Err(crate::session_inputs::InputError::Unavailable);
        }
        let sessions = self.sessions.borrow();
        if !sessions.contains_key(id) {
            return Err(crate::session_inputs::InputError::Unavailable);
        }
        Ok(self
            .inputs
            .write()
            .entry(id.to_owned())
            .or_default()
            .clone())
    }

    pub(crate) fn bind_inputs(&self, id: &str, store: crate::session_inputs::Store) {
        let sessions = self.sessions.borrow();
        let previous = self.inputs.write().insert(id.to_owned(), store.clone());
        drop(sessions);
        if let Some(previous) = previous.filter(|previous| !Arc::ptr_eq(previous, &store)) {
            previous.lock().close();
        }
    }
}

/// Removes the binding under the session lock, then closes that exact store
/// outside the lock. Cleanup for an old ID cannot close a reopened session.
pub(super) struct RetiredSession {
    id: String,
    inputs: Option<crate::session_inputs::Store>,
    spill_dir: Option<std::path::PathBuf>,
}

impl RetiredSession {
    pub(super) fn finish(mut self) -> String {
        self.close_inputs();
        crate::llm::agent_host_primitives::tool_result_budget::release_spill_dir(
            &self.id,
            self.spill_dir.take(),
        );
        std::mem::take(&mut self.id)
    }

    fn close_inputs(&mut self) {
        if let Some(inputs) = self.inputs.take() {
            inputs.lock().close();
        }
    }
}

impl Drop for RetiredSession {
    fn drop(&mut self) {
        self.close_inputs();
    }
}

pub(super) fn retire_session_state(
    sessions: &mut HashMap<String, SessionState>,
    id: &str,
) -> Option<RetiredSession> {
    sessions.remove(id)?;
    crate::run_tape::note_session_closed(id);
    Some(RetiredSession {
        id: id.to_owned(),
        inputs: active_session_runtime().inputs.write().remove(id),
        spill_dir: crate::llm::agent_host_primitives::tool_result_budget::take_session_spill_dir(
            id,
        ),
    })
}

pub(super) fn retire_all_session_states(
    sessions: &mut HashMap<String, SessionState>,
) -> Vec<RetiredSession> {
    let runtime = active_session_runtime();
    let mut inputs = std::mem::take(&mut *runtime.inputs.write());
    let mut retired = sessions
        .drain()
        .map(|(id, _)| {
            crate::run_tape::note_session_closed(&id);
            RetiredSession {
                inputs: inputs.remove(&id),
                spill_dir:
                    crate::llm::agent_host_primitives::tool_result_budget::take_session_spill_dir(
                        &id,
                    ),
                id,
            }
        })
        .collect::<Vec<_>>();
    retired.extend(inputs.into_iter().map(|(id, inputs)| RetiredSession {
        spill_dir: crate::llm::agent_host_primitives::tool_result_budget::take_session_spill_dir(
            &id,
        ),
        id,
        inputs: Some(inputs),
    }));
    retired
}

thread_local! {
    static ACTIVE_SESSION_RUNTIME: RefCell<Arc<AgentSessionRuntime>> =
        RefCell::new(fresh_session_runtime());
}

pub(crate) fn fresh_session_runtime() -> Arc<AgentSessionRuntime> {
    Arc::new(AgentSessionRuntime::default())
}

pub fn active_session_runtime() -> Arc<AgentSessionRuntime> {
    ACTIVE_SESSION_RUNTIME.with(|slot| Arc::clone(&slot.borrow()))
}

/// Record an unknown host event name in the runtime that owns the session.
/// The runtime is shared across worker-thread migration, unlike ambient
/// thread-local storage.
pub fn mark_unknown_host_event_warning(session_id: &str, event_type: &str) -> bool {
    active_session_runtime()
        .unknown_host_event_warnings
        .borrow_mut()
        .insert((session_id.to_string(), event_type.to_string()))
}

/// Record that this dropped payload key has been reported for this session.
///
/// Returns `true` the first time only, so a key dropped on every iteration of a
/// loop reports once instead of once per turn.
pub fn mark_dropped_host_payload_key_warning(
    session_id: &str,
    event_type: &str,
    key: &str,
) -> bool {
    active_session_runtime()
        .dropped_host_payload_key_warnings
        .borrow_mut()
        .insert((
            session_id.to_string(),
            event_type.to_string(),
            key.to_string(),
        ))
}

pub(super) fn clear_unknown_host_event_warnings(session_id: &str) {
    active_session_runtime()
        .unknown_host_event_warnings
        .borrow_mut()
        .retain(|(warned_session_id, _)| warned_session_id != session_id);
    active_session_runtime()
        .dropped_host_payload_key_warnings
        .borrow_mut()
        .retain(|(warned_session_id, _, _)| warned_session_id != session_id);
}

pub fn swap_active_session_runtime(next: Arc<AgentSessionRuntime>) -> Arc<AgentSessionRuntime> {
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

#[cfg(test)]
mod input_lifecycle_tests {
    use super::*;
    use crate::session_inputs::{self, InputKind, InputLifetime, InputSpec, InputUsage, InputUse};
    use std::collections::BTreeMap;

    fn secret_spec() -> InputSpec {
        InputSpec {
            name: "SERVICE_KEY".into(),
            kind: InputKind::Secret,
            lifetime: InputLifetime::Session,
            usage: InputUsage::SessionTools,
        }
    }

    fn grant(id: &str, value: &str) -> session_inputs::Store {
        session_inputs::put(id, secret_spec(), value.into(), false, 0).unwrap();
        active_session_runtime().input_store(id, false).unwrap()
    }

    fn assert_current_binding(id: &str, expected: &str) {
        let _current = crate::agent_sessions::enter_current_session(id.to_owned());
        let bindings = BTreeMap::from([("BOUND".into(), "SERVICE_KEY".into())]);
        assert_eq!(
            session_inputs::command_environment(bindings).unwrap()["BOUND"],
            expected
        );
    }

    #[test]
    fn session_input_retirement_cannot_close_a_concurrently_reopened_grant() {
        let id = crate::agent_sessions::open_or_create(None).unwrap();
        let old = grant(&id, "old-private-canary");
        assert_current_binding(&id, "old-private-canary");
        let runtime = active_session_runtime();
        let retired = retire_session_state(&mut runtime.sessions.borrow_mut(), &id).unwrap();
        assert!(!crate::agent_sessions::exists(&id));
        let ready = Arc::new(std::sync::Barrier::new(2));
        let closing = Arc::clone(&ready);
        let closer = crate::runtime_stack::spawn(move || {
            closing.wait();
            retired.finish();
        });
        crate::agent_sessions::open_or_create(Some(id.clone())).unwrap();
        let fresh = grant(&id, "fresh-private-canary");
        assert!(!Arc::ptr_eq(&old, &fresh));
        ready.wait();
        closer.join().unwrap();
        assert!(old.lock().is_closed());
        assert!(old
            .lock()
            .with_value("SERVICE_KEY", InputUse::Command, 0, str::to_owned)
            .is_err());
        assert_current_binding(&id, "fresh-private-canary");
        assert_eq!(runtime.inputs.read().len(), 1);
        assert!(crate::agent_sessions::close(&id));
    }

    #[test]
    fn session_input_reset_revokes_retained_and_staged_stores() {
        let id = crate::agent_sessions::open_or_create(None).unwrap();
        let retained = grant(&id, "reset-private-canary");
        let filter = {
            let _current = crate::agent_sessions::enter_current_session(id.clone());
            session_inputs::OutputFilter::capture()
        };
        let staged = session_inputs::Store::default();
        staged
            .lock()
            .put(secret_spec(), "staged-private-canary".into(), false, 0)
            .unwrap();
        session_inputs::bind("staged-before-admission", staged.clone());
        assert_eq!(active_session_runtime().inputs.read().len(), 2);
        crate::agent_sessions::reset_session_store();
        assert!(retained.lock().is_closed());
        assert!(staged.lock().is_closed());
        assert!(active_session_runtime().inputs.read().is_empty());
        assert_eq!(filter.redact("reset-private-canary"), "[redacted]");
        crate::agent_sessions::open_or_create(Some(id.clone())).unwrap();
        assert!(session_inputs::list(&id, 0).unwrap().is_empty());
        assert!(crate::agent_sessions::close(&id));
    }

    #[test]
    fn session_input_rebinding_preserves_the_same_store_and_retires_replacements() {
        let id = crate::agent_sessions::open_or_create(None).unwrap();
        let old = grant(&id, "original-private-canary");
        session_inputs::bind(&id, old.clone());
        assert_current_binding(&id, "original-private-canary");
        let replacement = session_inputs::Store::default();
        replacement
            .lock()
            .put(secret_spec(), "replacement-private-canary".into(), false, 0)
            .unwrap();
        session_inputs::bind(&id, replacement.clone());
        assert!(old.lock().is_closed());
        assert!(!replacement.lock().is_closed());
        assert_current_binding(&id, "replacement-private-canary");
        assert!(crate::agent_sessions::close(&id));
    }

    #[test]
    fn session_input_child_grants_remain_independent_of_the_parent() {
        let parent = crate::agent_sessions::open_or_create(None).unwrap();
        grant(&parent, "parent-private-canary");
        let child = crate::agent_sessions::fork(&parent, None).unwrap().unwrap();
        assert!(session_inputs::list(&child, 0).unwrap().is_empty());
        assert_current_binding(&parent, "parent-private-canary");
        grant(&child, "child-private-canary");
        assert!(crate::agent_sessions::close(&parent));
        assert_current_binding(&child, "child-private-canary");
        assert!(crate::agent_sessions::close(&child));
    }
}
