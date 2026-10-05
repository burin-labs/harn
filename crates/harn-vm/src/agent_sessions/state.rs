use super::*;

impl SessionState {
    pub(super) fn new(id: String) -> Self {
        let now = Instant::now();
        let transcript = empty_transcript(&id);
        Self {
            admission_role: SessionAdmissionRole::Admitted,
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
            transcript_budget_policy: default_transcript_budget_policy(),
            last_transcript_budget_action: None,
            live_clients: BTreeMap::new(),
            live_controller_id: None,
            completed_turn_checkpoints: Vec::new(),
            redo_stack: Vec::new(),
            text_tool_call_seq: 0,
            taint: Vec::new(),
            transcript_journal: None,
            revoked_reminder_ids: HashSet::new(),
            expired_reminder_ids: HashSet::new(),
            health: Box::default(),
        }
    }

    pub(super) fn touch(&mut self) {
        self.last_accessed = Instant::now();
    }

    pub(crate) fn replace_transcript(&mut self, transcript: VmValue) -> Result<(), String> {
        self.ensure_run_accepts_mutation("replace_transcript")?;
        if !crate::values_equal(&self.transcript, &transcript) {
            self.redo_stack.clear();
        }
        self.transcript = transcript;
        self.touch();
        Ok(())
    }

    /// Reject mutations once this run has queued its terminal boundary.
    /// The journal remains installed through recap projection so same-session
    /// admission stays closed, but it is sealed against work that could be
    /// queued behind the already-persisted terminal and then discarded.
    pub(crate) fn ensure_run_accepts_mutation(&self, action: &str) -> Result<(), String> {
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
