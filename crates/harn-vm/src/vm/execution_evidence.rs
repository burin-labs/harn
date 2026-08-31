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
            trace_spans: self
                .tracing_runtime
                .completed_spans_for_execution(&self.execution_id)
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
    struct TracingRuntimeGuard(std::sync::Arc<crate::tracing::TracingRuntime>);

    impl Drop for TracingRuntimeGuard {
        fn drop(&mut self) {
            crate::tracing::swap_active_tracing_runtime(self.0.clone());
        }
    }

    fn enter_tracing_runtime(
        runtime: std::sync::Arc<crate::tracing::TracingRuntime>,
    ) -> TracingRuntimeGuard {
        TracingRuntimeGuard(crate::tracing::swap_active_tracing_runtime(runtime))
    }

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

    #[test]
    fn vm_evidence_cannot_capture_the_callers_tracing_runtime() {
        let mut vm = super::Vm::new();
        vm.tracing_runtime = crate::tracing::fresh_tracing_runtime();
        {
            let _execution = crate::enter_execution_scope(vm.execution_id().clone());
            let _runtime = enter_tracing_runtime(vm.tracing_runtime.clone());
            crate::tracing::set_tracing_enabled(true);
            let span =
                crate::tracing::span_start(crate::tracing::SpanKind::Pipeline, "owned".to_string());
            crate::tracing::span_end(span);
        }

        let caller_runtime = crate::tracing::fresh_tracing_runtime();
        let caller_execution = crate::ExecutionId::mint();
        let _execution = crate::enter_execution_scope(caller_execution);
        let _caller = enter_tracing_runtime(caller_runtime);
        crate::tracing::set_tracing_enabled(true);
        let span =
            crate::tracing::span_start(crate::tracing::SpanKind::Pipeline, "caller".to_string());
        crate::tracing::span_end(span);

        let evidence = vm.execution_evidence(None, Vec::new());
        assert_eq!(
            evidence
                .trace_spans
                .iter()
                .map(|span| span.name.as_str())
                .collect::<Vec<_>>(),
            vec!["owned"]
        );
    }

    #[test]
    fn vm_evidence_selects_its_execution_from_a_reused_tracing_runtime() {
        let mut vm = super::Vm::new();
        vm.tracing_runtime = crate::tracing::fresh_tracing_runtime();
        let _runtime = enter_tracing_runtime(vm.tracing_runtime.clone());
        crate::tracing::set_tracing_enabled(true);

        let earlier_execution = crate::ExecutionId::mint();
        {
            let _execution = crate::enter_execution_scope(earlier_execution);
            let span = crate::tracing::span_start(
                crate::tracing::SpanKind::Pipeline,
                "earlier".to_string(),
            );
            crate::tracing::span_set_metadata(
                span,
                crate::tracing::meta::EXECUTION_ID,
                serde_json::json!(vm.execution_id()),
            );
            crate::tracing::span_end(span);
        }
        {
            let _execution = crate::enter_execution_scope(vm.execution_id().clone());
            let span = crate::tracing::span_start(
                crate::tracing::SpanKind::Pipeline,
                "current".to_string(),
            );
            crate::tracing::span_set_metadata(
                span,
                crate::tracing::meta::EXECUTION_ID,
                serde_json::json!("hxe-019c13e0-8080-7000-8000-000000000099"),
            );
            crate::tracing::span_end(span);
        }

        let evidence = vm.execution_evidence(None, Vec::new());
        assert_eq!(
            evidence
                .trace_spans
                .iter()
                .map(|span| span.name.as_str())
                .collect::<Vec<_>>(),
            vec!["current"]
        );
        assert_eq!(
            evidence.trace_spans[0].metadata[crate::tracing::meta::EXECUTION_ID],
            serde_json::json!(vm.execution_id())
        );
    }
}
