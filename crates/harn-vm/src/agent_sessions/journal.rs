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

pub(crate) fn pop_journal_event(id: &str) {
    super::SESSIONS.with(|sessions| {
        if let Some(journal) = sessions
            .borrow_mut()
            .get_mut(id)
            .and_then(|state| state.transcript_journal.as_mut())
        {
            journal.pop_front();
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
