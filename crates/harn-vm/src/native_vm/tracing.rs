//! Native trace classification for interpreter operations.

use crate::{
    tracing::SpanKind,
    vm::{ScopeSpan, VmSpanKind},
};

pub(super) fn start_span(kind: VmSpanKind, name: String) -> ScopeSpan {
    let kind = match kind {
        VmSpanKind::Pipeline => SpanKind::Pipeline,
        VmSpanKind::Import => SpanKind::Import,
        VmSpanKind::Parallel => SpanKind::Parallel,
        VmSpanKind::Spawn => SpanKind::Spawn,
    };
    begin(kind, name)
}

pub(super) fn builtin_span(name: &str) -> Option<ScopeSpan> {
    // Both public capability paths and ambient names use the live registry.
    let resolved = crate::stdlib::builtin_for_harness_path(name).unwrap_or(name);
    let kind = match resolved.strip_prefix("__cap_").unwrap_or(resolved) {
        "llm_call" | "llm_stream" | "llm_stream_call" | "agent_loop" => SpanKind::LlmCall,
        "mcp_call" => SpanKind::ToolCall,
        _ => return None,
    };
    if !crate::tracing::is_tracing_enabled() {
        return None;
    }
    Some(begin(kind, name.to_string()))
}

fn begin(kind: SpanKind, name: String) -> ScopeSpan {
    ScopeSpan::new(crate::tracing::span_start(kind, name), end)
}

fn end(id: u64) {
    crate::tracing::span_end(id);
}

#[cfg(test)]
mod tests {
    use crate::{value::VmValue, vm::Vm};

    #[tokio::test(flavor = "current_thread")]
    async fn builtin_observation_is_inert_when_disabled_and_classifies_both_public_paths() {
        crate::orchestration::scope_fresh_tracing_runtime(async {
            let mut vm = Vm::new();
            vm.register_builtin("llm_call", |_, _| Ok(VmValue::Int(7)));
            assert!(vm.runtime().builtin_span("llm_call").is_none());
            assert_eq!(
                vm.call_named_builtin("llm_call", vec![VmValue::String("fixture".into())])
                    .await
                    .unwrap()
                    .display(),
                "7"
            );
            assert!(crate::tracing::take_spans().is_empty());

            crate::tracing::set_tracing_enabled(true);
            assert_eq!(
                vm.call_named_builtin("llm_call", vec![VmValue::String("fixture".into())])
                    .await
                    .unwrap()
                    .display(),
                "7"
            );
            drop(
                vm.runtime()
                    .builtin_span("harness.llm.call")
                    .expect("live capability registry resolves the public path"),
            );
            assert!(vm.runtime().builtin_span("harness.stdio.println").is_none());
            let spans = crate::tracing::take_spans();
            assert_eq!(
                spans.len(),
                2,
                "both enabled observations reached the collector"
            );
            assert!(spans
                .iter()
                .all(|span| span.kind == crate::tracing::SpanKind::LlmCall));
            assert_eq!(spans[0].name, "llm_call");
            assert_eq!(spans[1].name, "harness.llm.call");
        })
        .await;
    }
}
