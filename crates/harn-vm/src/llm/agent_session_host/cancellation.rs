use crate::orchestration::NestedExecutionGuard;
use crate::value::VmError;

/// Owns a nested-policy push across the async agent lifecycle. Normal
/// finalization explicitly pops it while the session's ambient scope is
/// installed; cancellation merely disarms it because dropping the future
/// happens after that scope has been swapped out.
pub(super) struct CancelSafeNestedExecutionGuard(Option<NestedExecutionGuard>);

impl CancelSafeNestedExecutionGuard {
    pub(super) fn new(guard: NestedExecutionGuard) -> Self {
        Self(Some(guard))
    }

    pub(super) fn finish(mut self) {
        drop(self.0.take());
    }
}

impl Drop for CancelSafeNestedExecutionGuard {
    fn drop(&mut self) {
        if let Some(guard) = self.0.take() {
            guard.disarm();
        }
    }
}

/// Complete the cancellation-sensitive cleanup after finalization's last
/// await, while the owning ambient policy scope is still installed.
pub(super) fn finish_agent_session(
    session: &mut super::AgentHostSession,
    session_id: &str,
    abandon_in_flight: bool,
) {
    crate::llm::agent_runtime::fire_session_end_hooks(session_id, abandon_in_flight);
    if abandon_in_flight {
        crate::llm::agent_runtime::fire_session_close_hooks(session_id);
    }
    if let Some(guard) = session.nested_policy_guard.take() {
        guard.finish();
    }
}

/// Owns a host session while terminal projection is still fallible.
///
/// Finalization must not make the provider ledger or cleanup owner disappear
/// before the durable terminal append commits. Any error or cancellation
/// reinserts the exact run so Harn can measure it and retry finalization.
pub(super) struct AgentSessionFinalization {
    session_id: String,
    run_id: String,
    session: Option<super::AgentHostSession>,
}

impl AgentSessionFinalization {
    pub(super) fn take(session_id: &str) -> Result<Self, VmError> {
        let session = super::AGENT_HOST_SESSIONS
            .with(|sessions| sessions.borrow_mut().remove(session_id))
            .ok_or_else(|| {
                VmError::Runtime(format!(
                    "{}: unknown session `{session_id}`",
                    super::HOST_SESSION_FINALIZE
                ))
            })?;
        Ok(Self {
            session_id: session_id.to_string(),
            run_id: session.run_id.clone(),
            session: Some(session),
        })
    }

    pub(super) fn session_mut(&mut self) -> &mut super::AgentHostSession {
        self.session
            .as_mut()
            .expect("finalization owner always contains its session before commit")
    }

    pub(super) fn take_retry(session_id: &str, run_id: &str) -> Result<Self, VmError> {
        let session = super::AGENT_HOST_SESSIONS.with(|sessions| {
            let mut sessions = sessions.borrow_mut();
            let session = sessions.get(session_id).ok_or_else(|| {
                VmError::Runtime(format!(
                    "{}: unknown session `{session_id}`",
                    super::HOST_SESSION_FINALIZE
                ))
            })?;
            if session.run_id != run_id {
                return Err(VmError::Runtime(format!(
                    "{}: retry receipt names run `{run_id}`, but session `{session_id}` owns run `{}`",
                    super::HOST_SESSION_FINALIZE,
                    session.run_id
                )));
            }
            if session.pending_finalization.is_none() {
                return Err(VmError::Runtime(format!(
                    "{}: run `{run_id}` has no pending finalization",
                    super::HOST_SESSION_FINALIZE
                )));
            }
            sessions.remove(session_id).ok_or_else(|| {
                VmError::Runtime(format!(
                    "{}: session `{session_id}` disappeared while claiming retry `{run_id}`",
                    super::HOST_SESSION_FINALIZE
                ))
            })
        })?;
        Ok(Self {
            session_id: session_id.to_string(),
            run_id: session.run_id.clone(),
            session: Some(session),
        })
    }

    pub(super) fn commit(mut self) -> super::AgentHostSession {
        self.session
            .take()
            .expect("finalization owner commits exactly once")
    }
}

impl Drop for AgentSessionFinalization {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        super::AGENT_HOST_SESSIONS.with(|sessions| {
            let mut sessions = sessions.borrow_mut();
            if let Some(current) = sessions.get(&self.session_id) {
                crate::events::log_warn(
                    "agent.session_finalize_restore",
                    &format!(
                        "session={} run={} cannot restore over live run={}",
                        self.session_id, self.run_id, current.run_id
                    ),
                );
                return;
            }
            sessions.insert(self.session_id.clone(), session);
        });
    }
}

/// Cancellation-safe rollback for a session that crossed the host-registration
/// boundary but whose id has not yet been returned to Harn.
pub(super) struct AgentSessionInitRollback {
    session_id: String,
    owns_session: bool,
    armed: bool,
}

impl AgentSessionInitRollback {
    pub(super) fn new(session_id: String, owns_session: bool) -> Self {
        Self {
            session_id,
            owns_session,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }

    pub(super) async fn fail(&mut self) {
        if let Err(error) = super::live_transcript_journal::flush_init_terminal(
            &self.session_id,
            "failed",
            "session_initialization_failed",
        )
        .await
        {
            crate::events::log_warn(
                "agent.session_init_terminal_flush",
                &format!("session={} terminal flush error: {error}", self.session_id),
            );
            // Absence must not read as cleanup success. Keep the session,
            // queued terminal, and writer lease visible for a later retry.
            return;
        }
        self.rollback(true);
    }

    fn rollback(&mut self, finish_nested_policy: bool) {
        if !self.armed {
            return;
        }
        self.armed = false;
        let removed = super::AGENT_HOST_SESSIONS
            .with(|sessions| sessions.borrow_mut().remove(&self.session_id));
        crate::llm::permissions::clear_session_grants(&self.session_id);
        crate::orchestration::clear_approval_policy_repeat_counts(&self.session_id);

        if let Some(mut session) = removed {
            if let Some(frame) = session.current_session_frame.take() {
                crate::agent_sessions::remove_current_session(frame);
            }
            if let Some(frame) = session.transcript_dir_frame.take() {
                crate::llm::agent_observe::remove_llm_transcript_dir(frame);
            }
            if finish_nested_policy {
                finish_agent_session(&mut session, &self.session_id, true);
            } else {
                crate::llm::agent_runtime::fire_session_end_hooks(&self.session_id, true);
                crate::llm::agent_runtime::fire_session_close_hooks(&self.session_id);
            }
        }
        crate::agent_sessions::clear_journal(&self.session_id);
        if self.owns_session {
            crate::agent_sessions::close(&self.session_id);
        }
    }
}

impl Drop for AgentSessionInitRollback {
    fn drop(&mut self) {
        if self.armed {
            // Hard cancellation is completed by the owning VM task after its
            // join handle stops. Keep the exact journal, host session, and
            // writer lease visible until that async terminalization finishes.
            self.armed = false;
        }
    }
}

/// Release host-owned state after an embedding surface has cancelled and
/// awaited an agent future instead of letting it reach `finalize`.
pub(crate) async fn abandon_agent_session(session_id: &str) -> Result<(), VmError> {
    let has_host_session =
        super::AGENT_HOST_SESSIONS.with(|sessions| sessions.borrow().contains_key(session_id));
    if !has_host_session && !crate::agent_sessions::has_journal(session_id) {
        return Ok(());
    }

    // Terminalize through the same durable journal as ordinary finalization.
    // On failure, retain both owners so explicit cancellation can be retried.
    let owns_session = crate::agent_sessions::journal_owns_session(session_id)
        || super::AGENT_HOST_SESSIONS.with(|sessions| {
            sessions
                .borrow()
                .get(session_id)
                .is_some_and(|session| session.owns_session)
        });
    if crate::agent_sessions::has_journal(session_id) {
        let provider_call_count = super::AGENT_HOST_SESSIONS.with(|sessions| {
            sessions
                .borrow()
                .get(session_id)
                .map(|session| session.provider_call_count)
                .unwrap_or(0)
        });
        let terminal = crate::agent_events::AgentTerminalOutcome::new(
            crate::agent_events::classify_agent_terminal("cancelled", "cancelled", false, None),
            "cancelled",
        );
        super::live_transcript_journal::flush_terminal(
            session_id,
            "cancelled",
            "cancelled",
            None,
            None,
            &terminal,
            provider_call_count,
        )
        .await?;
    }
    let removed =
        super::AGENT_HOST_SESSIONS.with(|sessions| sessions.borrow_mut().remove(session_id));
    if let Some(mut session) = removed {
        if let Some(frame) = session.current_session_frame.take() {
            crate::agent_sessions::remove_current_session(frame);
        }
        if let Some(frame) = session.transcript_dir_frame.take() {
            crate::llm::agent_observe::remove_llm_transcript_dir(frame);
        }
        // The embedding scope has already been restored after cancellation;
        // dropping the host session intentionally disarms, rather than pops,
        // its nested-policy guard.
        drop(session);
    }
    crate::agent_sessions::clear_journal(session_id);
    crate::llm::permissions::clear_session_grants(session_id);
    crate::orchestration::clear_approval_policy_repeat_counts(session_id);
    crate::llm::agent_runtime::fire_session_end_hooks(session_id, true);
    crate::llm::agent_runtime::fire_session_close_hooks(session_id);
    if owns_session {
        crate::agent_sessions::close(session_id);
    }
    Ok(())
}

/// Finish every live agent session owned by one cancelled VM task.
pub(crate) async fn abandon_task_sessions(
    execution_id: &str,
    task_id: &str,
) -> Result<(), VmError> {
    let mut session_ids = crate::agent_sessions::journal_sessions_for_task(execution_id, task_id);
    let host_session_ids = super::AGENT_HOST_SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .values()
            .filter(|session| session.execution_id == execution_id && session.task_id == task_id)
            .map(|session| session.session_id.clone())
            .collect::<Vec<_>>()
    });
    for session_id in host_session_ids {
        if !session_ids.contains(&session_id) {
            session_ids.push(session_id);
        }
    }
    let mut failures = Vec::new();
    for session_id in session_ids {
        if let Err(error) = abandon_agent_session(&session_id).await {
            failures.push(format!("{session_id}: {error}"));
        }
    }
    if !failures.is_empty() {
        return Err(VmError::Runtime(format!(
            "agent task cancellation left {} terminal session(s) pending: {}",
            failures.len(),
            failures.join("; ")
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "cancellation_tests.rs"]
mod tests;
