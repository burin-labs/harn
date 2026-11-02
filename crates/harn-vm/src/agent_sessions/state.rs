use super::*;
use std::collections::HashMap;

impl SessionState {
    pub(super) fn new(id: String) -> Self {
        let now = Instant::now();
        let transcript = empty_transcript(&id);
        Self {
            admission_role: SessionAdmissionRole::Admitted,
            transcript_recording_ready: false,
            retention: Arc::new(()),
            id,
            transcript,
            subscribers: Vec::new(),
            created_at: crate::orchestration::now_unix_seconds_text(),
            last_accessed: now,
            parent_id: None,
            child_ids: Vec::new(),
            branched_at_event_index: None,
            actor_chain: None,
            active_skills: Vec::new(),
            tool_format: None,
            system_prompt: None,
            pinned_model: None,
            pinned_reasoning_policy: None,
            last_withdrawal_reason: None,
            workspace_policy: WorkspacePolicy::default(),
            workspace_anchor: None,
            scratchpad: None,
            scratchpad_version: 0,
            goal: SessionGoalState::default(),
            goal_pending_fork: false,
            transcript_budget_policy: default_transcript_budget_policy(),
            last_transcript_budget_action: None,
            live_clients: BTreeMap::new(),
            live_controller_id: None,
            completed_turn_checkpoints: Vec::new(),
            redo_stack: Vec::new(),
            text_tool_call_seq: 0,
            taint: Vec::new(),
            transcript_journal: None,
            canonical_store_dir: None,
            reset_in_flight: false,
            revoked_reminder_ids: HashSet::new(),
            expired_reminder_ids: HashSet::new(),
            health: Box::default(),
        }
    }

    pub(super) fn insert_into(mut self, sessions: &mut HashMap<String, Self>) {
        let std::collections::hash_map::Entry::Vacant(entry) = sessions.entry(self.id.clone())
        else {
            unreachable!("session admission cannot replace an existing session");
        };
        self.transcript_recording_ready = true;
        crate::run_tape::note_session_opened(&self.id, &self.transcript);
        entry.insert(self);
    }

    pub(super) fn touch(&mut self) {
        self.last_accessed = Instant::now();
    }

    pub(super) fn is_evictable(&self) -> bool {
        self.transcript_journal.is_none() && Arc::strong_count(&self.retention) == 1
    }

    pub(crate) fn replace_transcript(&mut self, transcript: VmValue) -> Result<(), String> {
        self.ensure_run_accepts_mutation("replace_transcript")?;
        if !crate::values_equal(&self.transcript, &transcript) {
            self.redo_stack.clear();
            if self.transcript_recording_ready {
                crate::run_tape::note_transcript_change(&self.id, &self.transcript, &transcript);
            }
        }
        self.transcript = transcript;
        self.touch();
        Ok(())
    }

    pub(super) fn reminder_state_snapshot(&self) -> crate::testbench::tape::ReminderStateSnapshot {
        crate::testbench::tape::ReminderStateSnapshot {
            revoked_ids: self.revoked_reminder_ids.iter().cloned().collect(),
            expired_ids: self.expired_reminder_ids.iter().cloned().collect(),
        }
    }

    /// Reject mutations once this run has queued its terminal boundary.
    /// The journal remains installed through recap projection so same-session
    /// admission stays closed, but it is sealed against work that could be
    /// queued behind the already-persisted terminal and then discarded.
    pub(crate) fn ensure_run_accepts_mutation(&self, action: &str) -> Result<(), String> {
        if self.reset_in_flight {
            return Err(format!(
                "session '{}' is being reset; {action} must wait for it",
                self.id
            ));
        }
        if self
            .transcript_journal
            .as_ref()
            .is_some_and(crate::agent_session_journal::JournalState::terminal_queued)
        {
            return Err(format!(
                "session '{}' is terminal; {action} cannot mutate it",
                self.id
            ));
        }
        Ok(())
    }
}
