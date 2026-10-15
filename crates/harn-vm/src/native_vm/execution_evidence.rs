use crate::native_vm::NativeVmState;
use crate::vm::Vm;

impl Vm {
    /// Install an explicit decision recording/replay scope. Child VMs inherit
    /// it, including work scheduled on other threads; misses cannot go live.
    pub fn set_evaluation_replay(
        &mut self,
        scope: crate::llm::decision::replay::EvaluationReplayScope,
    ) {
        self.native_mut().evaluation.replay = Some(scope);
    }

    /// Snapshot the canonical evidence envelope for this execution tree.
    /// Hosts provide persistence outcomes; Harn owns identity, schema, spans,
    /// and the relationship between those facts.
    pub fn execution_evidence(
        &self,
        flight_recording: Option<crate::flight_recorder::FlightRecordingArtifact>,
        gaps: Vec<crate::orchestration::RunEvidenceGapRecord>,
    ) -> crate::orchestration::ExecutionEvidenceRecord {
        let mut gaps = gaps;
        let (evaluation_receipts, evaluation_dropped) =
            self.native().evaluation.journal.lock().snapshot();
        if evaluation_dropped > 0 {
            gaps.push(crate::orchestration::RunEvidenceGapRecord {
                component: "evaluation_receipts".into(),
                code: "receipt_limit_exceeded".into(),
                message: format!("{evaluation_dropped} evaluation receipts beyond the first 1024 were not retained"),
            });
        }
        let dropped = crate::llm::dropped_reasoning_receipts();
        if let Some(gap) = crate::llm::reasoning_receipt::overflow_gap(dropped) {
            // A truncated list must not read as the whole run. The overflow is
            // named as a gap rather than left to look like a short run.
            gaps.push(gap);
        }
        crate::orchestration::ExecutionEvidenceRecord {
            evaluation_receipts: Some(evaluation_receipts),
            reasoning_receipts: Some(crate::llm::peek_reasoning_receipts()),
            schema_version: crate::orchestration::EXECUTION_EVIDENCE_SCHEMA_VERSION,
            execution_id: Some(self.execution_id.to_string()),
            trace_spans: self
                .native()
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
    use crate::native_vm::NativeVmState;
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
        vm.native_mut().tracing_runtime = crate::tracing::fresh_tracing_runtime();
        {
            let _execution = crate::enter_execution_scope(vm.execution_id().clone());
            let _runtime = enter_tracing_runtime(vm.native().tracing_runtime.clone());
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
        vm.native_mut().tracing_runtime = crate::tracing::fresh_tracing_runtime();
        let _runtime = enter_tracing_runtime(vm.native().tracing_runtime.clone());
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
