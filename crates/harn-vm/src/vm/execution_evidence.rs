use super::Vm;

impl Vm {
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
        if !self.owns_execution {
            return;
        }
        self.execution_id = crate::observability::execution_scope::mint_execution_scope();
        self.flight_recorder = self.flight_recorder_max_events.map(|max_events| {
            crate::flight_recorder::FlightRecorder::new(self.execution_id.clone(), max_events)
        });
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

    /// Snapshot the canonical evidence envelope for this execution tree.
    /// Hosts provide persistence outcomes; Harn owns identity, schema, spans,
    /// and the relationship between those facts.
    pub fn execution_evidence(
        &self,
        flight_recording: Option<crate::flight_recorder::FlightRecordingArtifact>,
        gaps: Vec<crate::orchestration::RunEvidenceGapRecord>,
    ) -> crate::orchestration::ExecutionEvidenceRecord {
        crate::orchestration::ExecutionEvidenceRecord {
            schema_version: crate::orchestration::EXECUTION_EVIDENCE_SCHEMA_VERSION,
            execution_id: Some(self.execution_id.to_string()),
            trace_spans: crate::tracing::peek_spans()
                .iter()
                .map(crate::orchestration::RunTraceSpanRecord::from)
                .collect(),
            flight_recording,
            gaps,
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn vm_owns_the_execution_evidence_envelope() {
        let vm = super::Vm::new();
        let gap = crate::orchestration::RunEvidenceGapRecord {
            component: "test".to_string(),
            code: "named_gap".to_string(),
            message: "named limit".to_string(),
        };

        let evidence = vm.execution_evidence(None, vec![gap.clone()]);

        assert_eq!(
            evidence.execution_id.as_deref(),
            Some(vm.execution_id().as_str())
        );
        assert_eq!(
            evidence.schema_version,
            crate::orchestration::EXECUTION_EVIDENCE_SCHEMA_VERSION
        );
        assert_eq!(evidence.gaps, vec![gap]);
    }
}
