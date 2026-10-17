//! Native route lookup and audit delivery for provider schema projection.

use serde_json::Value;

use harn_llm::schema_compat::{self, SchemaProjection};
pub(crate) use harn_llm::schema_compat::{SchemaCompatProfile, SchemaSurface};

pub(crate) fn sanitize_schema_for_provider(
    provider: &str,
    model: &str,
    profile: SchemaCompatProfile,
    surface: SchemaSurface,
    schema: &Value,
) -> Value {
    publish_projection(
        schema_compat::sanitize_schema(provider, model, profile, surface, schema),
        true,
    )
}

/// The same effective schema governs the wire request and response validation.
pub(crate) fn project_output_schema_for_provider(
    provider: &str,
    model: &str,
    strict: bool,
    schema: &Value,
    emit_receipts: bool,
) -> Value {
    let dialect = crate::llm::capabilities::lookup(provider, model).message_wire_format;
    publish_projection(
        schema_compat::project_output_schema(provider, model, dialect, strict, schema),
        emit_receipts,
    )
}

pub(crate) fn publish_wire_request(request: harn_llm::wire::EncodedRequest) -> Value {
    for metadata in request.schema_audits {
        publish_audit(metadata);
    }
    request.body
}

fn publish_projection(projection: SchemaProjection, emit_receipts: bool) -> Value {
    if emit_receipts {
        if let Some(metadata) = projection.audit_metadata() {
            publish_audit(metadata);
        }
    }
    projection.schema
}

pub(crate) fn publish_audit(metadata: std::collections::BTreeMap<String, Value>) {
    crate::events::log_debug_meta(
        "llm.schema_compat",
        "sanitized provider-facing JSON Schema",
        metadata,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_builders_publish_the_shared_encoder_audit_once() {
        let mut request = harn_llm::wire::openai::test_support::base_request_payload();
        request.provider = "openai".into();
        request.model = "gpt-4o".into();
        request.output_format = crate::llm::api::OutputFormat::JsonSchema {
            schema: serde_json::json!({"type": "string", "default": "removed"}),
            strict: true,
        };
        let structured = request.output_format.clone();
        for responses in [false, true] {
            let build = if responses {
                super::super::openai_responses::OpenAiResponsesProvider::build_request_body
            } else {
                super::super::openai_compat::OpenAiCompatibleProvider::build_request_body
            };
            let field = if responses { "text" } else { "response_format" };
            request.output_format = structured.clone();
            let sink = std::rc::Rc::new(crate::events::CollectorSink::new());
            crate::events::clear_event_sinks();
            crate::events::add_event_sink(sink.clone());
            let body = build(&request);
            let schema = if responses {
                &body["text"]["format"]["schema"]
            } else {
                &body["response_format"]["json_schema"]["schema"]
            };
            assert!(
                schema.is_object(),
                "builder did not encode structured output: {body}"
            );
            assert!(schema.get("default").is_none());
            {
                let logs = sink.logs.borrow();
                let audits: Vec<_> = logs
                    .iter()
                    .filter(|event| event.category == "llm.schema_compat")
                    .collect();
                assert_eq!(audits.len(), 1);
                assert_eq!(audits[0].metadata["model"], "gpt-4o");
                assert_eq!(audits[0].metadata["surface"], "structured_output");
                assert!(audits[0].metadata["changes"]
                    .as_array()
                    .is_some_and(|changes| !changes.is_empty()));
            }
            request.output_format = crate::llm::api::OutputFormat::Text;
            let plain = build(&request);
            assert!(plain.get(field).is_none());
            assert_eq!(
                sink.logs
                    .borrow()
                    .iter()
                    .filter(|event| event.category == "llm.schema_compat")
                    .count(),
                1
            );
            crate::events::reset_event_sinks();
        }
    }

    #[test]
    fn native_adapter_emits_the_lower_projection_receipt_once() {
        let sink = std::rc::Rc::new(crate::events::CollectorSink::new());
        crate::events::clear_event_sinks();
        crate::events::add_event_sink(sink.clone());
        let schema = serde_json::json!({"type": "string", "default": "original"});
        let expected = schema_compat::sanitize_schema(
            "openai",
            "fixture",
            SchemaCompatProfile::OpenAiStrict,
            SchemaSurface::ToolParameters,
            &schema,
        );
        let projected = sanitize_schema_for_provider(
            "openai",
            "fixture",
            SchemaCompatProfile::OpenAiStrict,
            SchemaSurface::ToolParameters,
            &schema,
        );
        assert_eq!(projected, expected.schema);
        {
            let logs = sink.logs.borrow();
            let events: Vec<_> = logs
                .iter()
                .filter(|event| event.category == "llm.schema_compat")
                .collect();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].metadata, expected.audit_metadata().unwrap());
        }
        // The validator's projection must not emit a second delivery receipt.
        let _ = publish_projection(expected, false);
        assert_eq!(
            sink.logs
                .borrow()
                .iter()
                .filter(|event| event.category == "llm.schema_compat")
                .count(),
            1
        );
        // Already-compatible schemas also produce no audit event.
        let _ = sanitize_schema_for_provider(
            "openai",
            "fixture",
            SchemaCompatProfile::OpenAiStrict,
            SchemaSurface::ToolParameters,
            &projected,
        );
        assert_eq!(
            sink.logs
                .borrow()
                .iter()
                .filter(|event| event.category == "llm.schema_compat")
                .count(),
            1
        );
        crate::events::reset_event_sinks();
    }
}
