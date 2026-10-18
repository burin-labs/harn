//! Session-owned accessors for the live transcript mutation journal.

use crate::value::VmError;

pub(crate) fn journal_audit_writer(
    id: &str,
) -> Result<Option<crate::agent_session_journal::JournalAuditWriter>, String> {
    super::SESSIONS.with(|sessions| {
        let sessions = sessions.borrow();
        let state = sessions
            .get(id)
            .ok_or_else(|| format!("agent transcript audit writer: unknown session `{id}`"))?;
        state.ensure_run_accepts_mutation("journal_audit_writer")?;
        Ok(state
            .transcript_journal
            .as_ref()
            .map(crate::agent_session_journal::JournalState::audit_writer))
    })
}

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

pub(crate) fn journal_flush_lock(id: &str) -> Option<std::sync::Arc<tokio::sync::Mutex<()>>> {
    super::SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .get(id)
            .and_then(|state| state.transcript_journal.as_ref())
            .map(crate::agent_session_journal::JournalState::flush_lock)
    })
}

#[cfg(test)]
pub(crate) fn next_journal_event(
    id: &str,
) -> Result<
    Option<(
        crate::stdlib::canonical_store::CanonicalStore,
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

/// Every queued journal mutation as the events one flush appends, or `None`
/// when the session has no journal.
pub(crate) fn pending_journal_events(
    id: &str,
) -> Result<Option<crate::agent_session_journal::PendingJournalBatch>, VmError> {
    super::SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .get(id)
            .and_then(|state| state.transcript_journal.as_ref())
            .map(crate::agent_session_journal::JournalState::pending_events)
            .transpose()
    })
}

pub(crate) fn record_persisted_journal_events(id: &str, event_ids: &[u64]) {
    super::SESSIONS.with(|sessions| {
        if let Some(journal) = sessions
            .borrow_mut()
            .get_mut(id)
            .and_then(|state| state.transcript_journal.as_mut())
        {
            journal.record_persisted_events(event_ids);
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

pub(crate) fn with_run_preparation<R>(
    id: &str,
    f: impl FnOnce(&mut crate::agent_session_journal::RunPreparation) -> R,
) -> Option<R> {
    super::SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let preparation = sessions
            .get_mut(id)?
            .transcript_journal
            .as_mut()?
            .preparation
            .as_mut()?;
        Some(f(preparation))
    })
}

pub(crate) fn prepare_run(
    id: &str,
    has_canonical_history: bool,
    state_dir: crate::stdlib::session_store::SessionStoreDir,
) -> Result<(), VmError> {
    super::SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let journal = sessions
            .get_mut(id)
            .and_then(|state| state.transcript_journal.as_mut())
            .ok_or_else(|| VmError::Runtime("run preparation has no active journal".into()))?;
        journal.preparation = Some(crate::agent_session_journal::RunPreparation {
            adopted: false,
            has_canonical_history,
            state_dir,
            accounting: Default::default(),
        });
        Ok(())
    })
}

pub(crate) fn adopt_prepared_run(
    id: &str,
    execution_id: &str,
    task_id: &str,
    options: &crate::value::DictMap,
) -> Result<Option<(String, bool, bool)>, VmError> {
    super::SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        let Some(journal) = sessions
            .get_mut(id)
            .and_then(|state| state.transcript_journal.as_mut())
        else {
            return Ok(None);
        };
        let Some(preparation) = journal.preparation.as_ref() else {
            return Ok(None);
        };
        if journal.execution_id() != Some(execution_id) || journal.task_id() != Some(task_id) {
            return Err(VmError::Runtime(
                "prepared run belongs to a different execution task".into(),
            ));
        }
        if preparation.adopted {
            return Err(VmError::Runtime(
                "prepared run already adopted by an agent loop".into(),
            ));
        }
        if let Some(crate::value::VmValue::String(requested)) = options.get("run_id") {
            if !requested.trim().is_empty() && requested.as_str() != journal.run_id() {
                return Err(VmError::Runtime(
                    "agent loop run_id differs from its prepared run".into(),
                ));
            }
        }
        if preparation.state_dir
            != crate::stdlib::session_store::canonical_store_state_dir(Some(options))?
        {
            return Err(VmError::Runtime(
                "agent loop store root differs from its prepared run".into(),
            ));
        }
        journal.set_workflow_learning_eligibility(options)?;
        let preparation = journal.preparation.as_mut().expect("validated preparation");
        preparation.adopted = true;
        let has_canonical_history = preparation.has_canonical_history;
        Ok(Some((
            journal.run_id().to_string(),
            has_canonical_history,
            journal.owns_session(),
        )))
    })
}

pub(crate) fn take_preparation_accounting(
    id: &str,
) -> crate::agent_session_journal::PreparationAccounting {
    super::SESSIONS.with(|sessions| {
        sessions
            .borrow_mut()
            .get_mut(id)
            .and_then(|state| state.transcript_journal.as_mut())
            .and_then(|journal| journal.preparation.take())
            .map(|preparation| preparation.accounting)
            .unwrap_or_default()
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
pub(crate) fn journal_store(id: &str) -> Option<crate::stdlib::canonical_store::CanonicalStore> {
    super::SESSIONS.with(|sessions| {
        sessions
            .try_borrow()
            .ok()?
            .get(id)
            .and_then(|state| state.transcript_journal.as_ref())
            .map(crate::agent_session_journal::JournalState::store)
    })
}
