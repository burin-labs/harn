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
        harn_session_store::SqliteSessionStore,
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
pub(crate) fn journal_store(id: &str) -> Option<harn_session_store::SqliteSessionStore> {
    super::SESSIONS.with(|sessions| {
        sessions
            .try_borrow()
            .ok()?
            .get(id)
            .and_then(|state| state.transcript_journal.as_ref())
            .map(crate::agent_session_journal::JournalState::store)
    })
}
