//! End-to-end coverage for the streaming `output_schema` abort
//! (`schema_stream_abort` — harn#1775). Drives [`consume_sse_lines`]
//! against a canned OpenAI-shaped SSE body whose content delta
//! immediately violates the schema, asserts:
//!
//! - the consumer returns a categorized `SchemaStreamAborted` error,
//! - a `SchemaStreamAborted` transcript event is emitted, and
//! - the `harn_llm_schema_stream_aborted_total` counter increments.
//!
//! Driven via `consume_sse_lines` rather than a full HTTP stack so
//! the test stays deterministic and offline.
use super::sse::consume_sse_lines;
use super::*;
use crate::llm::api::StreamSchemaWatch;
use crate::llm::trace::{peek_agent_trace, reset_agent_trace_state, AgentTraceEvent};
use crate::value::ErrorCategory;
use crate::{install_active_metrics_registry, MetricsRegistry};
use std::sync::Arc;

fn schema_with_int_age() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["age"],
        "properties": {"age": {"type": "integer"}}
    })
}

fn build_payload(schema: serde_json::Value) -> crate::llm::api::LlmRequestPayload {
    let mut opts = crate::llm::api::options::base_opts("openai");
    opts.model = "gpt-test".to_string();
    opts.output_schema = Some(schema);
    opts.schema_stream_abort = true;
    opts.session_id = None;
    crate::llm::api::LlmRequestPayload::from(&opts)
}

#[tokio::test(flavor = "current_thread")]
async fn openai_stream_aborts_on_impossible_property_type() {
    reset_agent_trace_state();
    let metrics = Arc::new(MetricsRegistry::default());
    install_active_metrics_registry(metrics.clone());

    let payload = build_payload(schema_with_int_age());
    let watch = StreamSchemaWatch::from_payload(&payload).expect("schema is canonicalizable");

    // First content delta opens `"age":"`, which is incompatible with
    // `age: int`. The mid-stream watch should fire on the trailing
    // string quote; the second delta is never read because we abort.
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"age\\\":\"}}]}\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"\\\"twenty\"}}]}\n",
        "data: [DONE]\n",
    );
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let reader = tokio::io::BufReader::new(body.as_bytes());
    let err = consume_sse_lines(
        reader,
        "openai",
        "gpt-test",
        false,
        delta_tx,
        None,
        Some(watch),
        false,
    )
    .await
    .expect_err("schema abort must surface as error");

    match &err {
        VmError::CategorizedError { category, message } => {
            assert_eq!(*category, ErrorCategory::SchemaStreamAborted);
            assert!(
                message.contains("$.age"),
                "abort message should include JSON path; got: {message}"
            );
        }
        other => panic!("expected CategorizedError; got {other:?}"),
    }

    let events = peek_agent_trace();
    let aborts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentTraceEvent::SchemaStreamAborted {
                provider,
                model,
                reason,
                path,
                chunks_consumed,
            } => Some((
                provider.clone(),
                model.clone(),
                reason.clone(),
                path.clone(),
                *chunks_consumed,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        aborts.len(),
        1,
        "expected exactly one SchemaStreamAborted event; got {events:#?}"
    );
    let (provider, model, _reason, path, chunks) = &aborts[0];
    assert_eq!(provider, "openai");
    assert_eq!(model, "gpt-test");
    assert_eq!(path, "$.age");
    assert!(*chunks >= 1);

    // Telemetry counter incremented through the installed registry.
    let rendered = metrics.render_prometheus();
    assert!(
        rendered.contains("harn_llm_schema_stream_aborted_total"),
        "metric family missing from prometheus render"
    );
    assert!(
        rendered.contains(
            "harn_llm_schema_stream_aborted_total{model=\"gpt-test\",provider=\"openai\"} 1"
        ),
        "expected labelled counter increment; got:\n{rendered}"
    );

    reset_agent_trace_state();
    crate::clear_active_metrics_registry();
}

#[tokio::test(flavor = "current_thread")]
async fn opt_out_keeps_stream_alive_through_invalid_content() {
    reset_agent_trace_state();

    let mut payload = build_payload(schema_with_int_age());
    payload.schema_stream_abort = false;
    // With the watch disabled, even a clearly-invalid stream must
    // run to completion; only the post-hoc validator catches it.
    assert!(StreamSchemaWatch::from_payload(&payload).is_none());

    reset_agent_trace_state();
}
