#[cfg(test)]
#[cfg(test)]
#[cfg(test)]
mod tests {
    use crate::native_vm::NativeVmState;
    use crate::NativeVmExt;
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
        let vm = crate::new_vm();
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
        let mut vm = crate::new_vm();
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
        let mut vm = crate::new_vm();
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
