//! Session-owned accessors for the live transcript mutation journal.

use crate::value::VmError;

pub(crate) fn install_journal(
    id: &str,
    journal: crate::agent_session_journal::JournalState,
) -> Result<(), VmError> {
    super::SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let state = sessions.get_mut(id).ok_or_else(|| {
            VmError::Runtime(format!("agent transcript journal: unknown session `{id}`"))
        })?;
        if state.transcript_journal.is_some() {
            return Err(VmError::Runtime(format!(
                "agent transcript journal for `{id}` already has an active journal"
            )));
        }
        state.transcript_journal = Some(journal);
        Ok(())
    })
}

pub(crate) fn next_journal_event(
    id: &str,
) -> Result<
    Option<(
        crate::stdlib::session_store::CanonicalStore,
        harn_session_store::AppendEvent,
    )>,
    VmError,
> {
    super::SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .get(id)
            .and_then(|state| state.transcript_journal.as_ref())
            .map(crate::agent_session_journal::JournalState::next_event)
            .transpose()
            .map(Option::flatten)
    })
}

pub(crate) fn record_persisted_journal_event(id: &str, event_id: u64) {
    super::SESSIONS.with(|sessions| {
        if let Some(journal) = sessions
            .borrow_mut()
            .get_mut(id)
            .and_then(|state| state.transcript_journal.as_mut())
        {
            journal.record_persisted_event(event_id);
        }
    });
}

pub(crate) fn clear_journal(id: &str) {
    super::SESSIONS.with(|sessions| {
        if let Some(state) = sessions.borrow_mut().get_mut(id) {
            state.transcript_journal = None;
        }
    });
}

pub(crate) fn claim_journal_task(
    id: &str,
    execution_id: &str,
    task_id: String,
    owns_session: bool,
) -> Result<(), VmError> {
    super::SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let claimed = sessions
            .values()
            .filter_map(|state| state.transcript_journal.as_ref())
            .filter(|journal| journal.is_claimed())
            .count();
        let cap = super::SESSION_CAP.with(|limit| limit.get());
        if claimed >= cap {
            return Err(VmError::Runtime(format!(
                "agent lifecycle admission refused before session start: pending={claimed} limit={cap}"
            )));
        }
        let journal = sessions
            .get_mut(id)
            .and_then(|state| state.transcript_journal.as_mut())
            .ok_or_else(|| {
                VmError::Runtime(format!(
                    "agent transcript journal: session `{id}` has no active journal"
                ))
            })?;
        journal.claim_task(execution_id, id, task_id, owns_session)
    })
}

pub(crate) fn journal_sessions_for_task(execution_id: &str, task_id: &str) -> Vec<String> {
    super::SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .iter()
            .filter(|(_, state)| {
                state.transcript_journal.as_ref().is_some_and(|journal| {
                    journal.execution_id() == Some(execution_id)
                        && journal.task_id() == Some(task_id)
                })
            })
            .map(|(session_id, _)| session_id.clone())
            .collect()
    })
}

pub(crate) fn journal_owns_session(id: &str) -> bool {
    super::SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .get(id)
            .and_then(|state| state.transcript_journal.as_ref())
            .is_some_and(crate::agent_session_journal::JournalState::owns_session)
    })
}

pub(crate) fn has_journal(id: &str) -> bool {
    super::SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .get(id)
            .is_some_and(|state| state.transcript_journal.is_some())
    })
}

/// Run identity owned by the live transcript journal for this session.
pub(crate) fn active_run_id(id: &str) -> Option<String> {
    super::SESSIONS.with(|sessions| {
        sessions
            .try_borrow()
            .ok()?
            .get(id)
            .and_then(|state| state.transcript_journal.as_ref())
            .map(|journal| journal.run_id().to_string())
    })
}

/// First durable event written by the active invocation.
pub(crate) fn journal_first_event_id(id: &str) -> Option<u64> {
    super::SESSIONS.with(|sessions| {
        sessions
            .try_borrow()
            .ok()?
            .get(id)
            .and_then(|state| state.transcript_journal.as_ref())
            .and_then(crate::agent_session_journal::JournalState::first_event_id)
    })
}

/// Canonical store owned by the live journal for a terminal read-back.
///
/// The clone is another handle to the same SQLite store, not a second source of
/// truth. Callers use it only after flushing queued mutations.
pub(crate) fn journal_store(id: &str) -> Option<crate::stdlib::session_store::CanonicalStore> {
    super::SESSIONS.with(|sessions| {
        sessions
            .try_borrow()
            .ok()?
            .get(id)
            .and_then(|state| state.transcript_journal.as_ref())
            .map(crate::agent_session_journal::JournalState::store)
    })
}
