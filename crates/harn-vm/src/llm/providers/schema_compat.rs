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
    for projection in request.schema_projections {
        publish_projection(projection, true);
    }
    request.body
}

fn publish_projection(projection: SchemaProjection, emit_receipts: bool) -> Value {
    if emit_receipts {
        if let Some(metadata) = projection.audit_metadata() {
            crate::events::log_debug_meta(
                "llm.schema_compat",
                "sanitized provider-facing JSON Schema",
                metadata,
            );
        }
    }
    projection.schema
}

#[cfg(test)]
mod tests {
    use super::*;

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
