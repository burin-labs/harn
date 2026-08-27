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
use crate::llm::api::DialectContract;
use crate::llm::api::StreamSchemaWatch;
use crate::llm::capabilities::WireDialect;
use crate::llm::trace::{peek_agent_trace, reset_agent_trace_state, AgentTraceEvent};
use crate::value::{ErrorCategory, VmValue};
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

fn build_strict_payload(schema: serde_json::Value) -> crate::llm::api::LlmRequestPayload {
    let mut opts = crate::llm::api::options::base_opts("openai");
    opts.model = "gpt-test".to_string();
    opts.output_schema = Some(schema.clone());
    opts.output_format = crate::llm::api::OutputFormat::JsonSchema {
        schema,
        strict: true,
    };
    opts.schema_stream_abort = true;
    opts.session_id = None;
    crate::llm::api::LlmRequestPayload::from(&opts)
}

fn assert_schema_failure(err: &VmError, kind: &str, path: &str, detail_fragment: &str) {
    let VmValue::Dict(caught) = err.thrown_value() else {
        panic!("schema abort must lower to a structured caught value: {err:?}");
    };
    let Some(VmValue::Dict(cause)) = caught.get("schema_failure") else {
        panic!("caught schema abort must retain its typed cause: {caught:?}");
    };
    assert_eq!(
        cause.get("kind").map(VmValue::display).as_deref(),
        Some(kind)
    );
    assert_eq!(
        cause.get("path").map(VmValue::display).as_deref(),
        Some(path)
    );
    assert!(
        cause
            .get("detail")
            .map(VmValue::display)
            .is_some_and(|detail| detail.contains(detail_fragment)),
        "schema cause must retain the validator detail: {cause:?}"
    );
}

#[test]
fn explicit_output_schema_owns_the_structured_request_contract() {
    let mut opts = crate::llm::api::options::base_opts("openai");
    opts.model = "gpt-test".to_string();
    opts.output_schema = Some(schema_with_int_age());
    opts.output_format = crate::llm::api::OutputFormat::JsonSchema {
        schema: serde_json::json!({"type": "object"}),
        strict: true,
    };

    let payload = crate::llm::api::LlmRequestPayload::from(&opts);
    let validation_schema = payload
        .output_schema
        .as_ref()
        .expect("structured output keeps a stream-validation schema");
    assert_eq!(
        validation_schema["properties"]["age"]["type"], "integer",
        "the explicit schema must remain the stream-validation contract"
    );
    assert_eq!(
        payload.output_format.schema(),
        Some(validation_schema),
        "the provider request must use the same caller-selected schema"
    );

    let data = crate::stdlib::json_to_vm_value(&serde_json::json!({"age": "twenty"}));
    assert!(
        !crate::llm::call::compute_validation_errors(&data, &opts).is_empty(),
        "post-stream validation must retain the explicit integer constraint"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn stream_validation_uses_the_schema_sent_to_the_provider() {
    reset_agent_trace_state();

    let schema = serde_json::json!({
        "type": "object",
        "required": ["detail"],
        "properties": {
            "detail": {"type": "string", "minLength": 1, "maxLength": 240}
        }
    });
    let payload = build_strict_payload(schema);
    let validation_schema = payload
        .output_schema
        .as_ref()
        .expect("strict output keeps a validation schema");
    assert!(
        validation_schema["properties"]["detail"]
            .get("maxLength")
            .is_none(),
        "local validation must use the provider-compatible schema"
    );
    let wire_body = crate::llm::providers::OpenAiCompatibleProvider::build_request_body(&payload);
    assert_eq!(
        wire_body["response_format"]["json_schema"]["schema"], *validation_schema,
        "stream validation and the provider request must share one schema"
    );

    let watch = StreamSchemaWatch::from_payload(&payload).expect("schema is canonicalizable");
    let detail = "x".repeat(242);
    let frame = serde_json::json!({
        "choices": [{"delta": {"content": format!("{{\"detail\":\"{detail}\"}}")}}]
    });
    let body = format!("data: {frame}\n\ndata: [DONE]\n\n");
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let reader = tokio::io::BufReader::new(body.as_bytes());
    consume_sse_lines(
        reader,
        "openai",
        "gpt-test",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        Some(watch),
        false,
    )
    .await
    .expect("a response valid against the wire schema must survive streaming");

    assert!(
        !peek_agent_trace()
            .iter()
            .any(|event| matches!(event, AgentTraceEvent::SchemaStreamAborted { .. })),
        "a provider-compatible response must not emit a schema abort"
    );
    reset_agent_trace_state();
}

#[test]
fn post_stream_validation_restores_the_caller_schema() {
    let schema = serde_json::json!({
        "type": "object",
        "required": ["detail"],
        "properties": {
            "detail": {"type": "string", "minLength": 1, "maxLength": 240}
        }
    });
    let mut opts = crate::llm::api::options::base_opts("openai");
    opts.model = "gpt-test".to_string();
    opts.output_schema = Some(schema.clone());
    opts.output_format = crate::llm::api::OutputFormat::JsonSchema {
        schema,
        strict: true,
    };
    let data = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "detail": "x".repeat(242)
    }));

    assert!(
        !crate::llm::call::compute_validation_errors(&data, &opts).is_empty(),
        "completed-response validation must restore the caller's length rule"
    );
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
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        Some(watch),
        false,
    )
    .await
    .expect_err("schema abort must surface as error");

    assert_schema_failure(&err, "wrong_type", "$.age", "expected type 'int'");

    assert_eq!(
        crate::value::error_to_category(&err),
        ErrorCategory::SchemaStreamAborted
    );
    assert!(
        err.to_string().contains("$.age"),
        "abort message should include JSON path; got: {err}"
    );

    let events = peek_agent_trace();
    let aborts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentTraceEvent::SchemaStreamAborted {
                provider,
                model,
                reason_kind,
                reason,
                path,
                chunks_consumed,
            } => Some((
                provider.clone(),
                model.clone(),
                reason_kind.clone(),
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
    let (provider, model, reason_kind, _reason, path, chunks) = &aborts[0];
    assert_eq!(provider, "openai");
    assert_eq!(model, "gpt-test");
    assert_eq!(reason_kind, "wrong_type");
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
async fn stream_abort_cause_distinguishes_missing_required_from_wrong_type() {
    reset_agent_trace_state();
    let schema = serde_json::json!({
        "type": "object",
        "required": ["detail"],
        "properties": {"detail": {"type": "string"}}
    });
    let payload = build_payload(schema);
    let watch = StreamSchemaWatch::from_payload(&payload).expect("schema is canonicalizable");
    let frame = serde_json::json!({
        "choices": [{"delta": {"content": "{}"}}]
    });
    let body = format!("data: {frame}\n\ndata: [DONE]\n\n");
    let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let reader = tokio::io::BufReader::new(body.as_bytes());
    let err = consume_sse_lines(
        reader,
        "openai",
        "gpt-test",
        DialectContract::new(WireDialect::OpenAiCompat, None),
        delta_tx,
        None,
        Some(watch),
        false,
    )
    .await
    .expect_err("missing required property must abort the stream");

    assert_schema_failure(&err, "missing_required", "$", "required");
    reset_agent_trace_state();
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
