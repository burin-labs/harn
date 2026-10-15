use super::Vm;

impl Vm {
    /// Prepare explicit host work under this interpreter's execution owner.
    pub fn prepare_host_call(&mut self) -> Result<super::AsyncBuiltinCtx, crate::VmError> {
        self.ensure_execution_available()?;
        self.prepare_execution_for_top_level();
        Ok(super::AsyncBuiltinCtx::from_inline_parent(self))
    }

    /// Enable exact, value-free source-path recording for this VM execution
    /// tree. Child VMs inherit the same bounded recorder.
    pub fn enable_flight_recorder(&mut self, max_events: usize) {
        self.flight_recorder_max_events = Some(max_events.max(1));
        self.flight_recorder = Some(crate::flight_recorder::FlightRecorder::new(
            self.execution_id.clone(),
            max_events,
        ));
    }

    pub(crate) fn prepare_execution_for_top_level(&mut self) {
        if self.owns_execution {
            self.execution_id = crate::observability::execution_scope::mint_execution_scope();
            self.flight_recorder = self.flight_recorder_max_events.map(|max_events| {
                crate::flight_recorder::FlightRecorder::new(self.execution_id.clone(), max_events)
            });
        }
        let execution_id = self.execution_id.clone();
        let new_execution = self.owns_execution;
        self.runtime_mut()
            .prepare_top_level(&execution_id, new_execution);
    }

    /// Durable identity of the current or most recently completed execution.
    pub fn execution_id(&self) -> &crate::ExecutionId {
        &self.execution_id
    }

    /// Snapshot the current flight recording without disabling it.
    pub fn flight_recording(&self) -> Option<crate::flight_recorder::FlightRecording> {
        self.flight_recorder
            .as_ref()
            .map(|recorder| recorder.snapshot())
    }
}
