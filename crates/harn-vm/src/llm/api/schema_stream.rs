//! Mid-stream `output_schema` enforcement for `llm_call`.
//!
//! When a script-driven `llm_call` carries `output_schema` and
//! `schema_stream_abort` is on (the default), the streaming transport
//! feeds every visible text delta through a [`StreamSchemaWatch`] before
//! handing it to the caller. The watch wraps the incremental validator
//! from `stdlib::json_stream` and, the first time the partial JSON can
//! no longer satisfy the schema, emits a `SchemaStreamAborted` transcript
//! event, increments the `harn_llm_schema_stream_aborted_total` counter,
//! and surfaces a categorized error so the schema-retry loop can react
//! one round trip earlier than `schema_retries` alone would allow.
//!
//! The watch is provider-agnostic: SSE (`consume_sse_lines`), NDJSON
//! (`consume_ollama_ndjson_lines`), and the in-process `FakeLlmProvider`
//! all route through the same helper so behavior — and tests — line up.

use crate::llm::trace::{emit_agent_event, AgentTraceEvent};
use crate::stdlib::json_stream::{JsonStreamStatus, StreamSchemaValidator};
use crate::value::VmDictExt;
use crate::value::{VmError, VmValue};

pub(crate) use crate::value::SchemaStreamAbort;

use super::options::LlmRequestPayload;

/// Mid-stream validator wired up by the LLM streaming transports.
///
/// Each text delta is fed through [`Self::observe`]. As soon as the
/// validator reaches `Invalid`, the watch records the abort, emits a
/// trace event, increments the labelled telemetry counter, and returns
/// `Some(SchemaStreamAbort)` so the transport can short-circuit the
/// provider connection.
pub(crate) struct StreamSchemaWatch {
    validator: StreamSchemaValidator,
    provider: String,
    model: String,
    chunks_consumed: usize,
    /// Once the abort fires we surface it exactly once; further deltas
    /// are dropped without re-emitting events or recounting metrics.
    fired: bool,
}

impl StreamSchemaWatch {
    /// Build a watch from a `LlmRequestPayload`. Returns `None` when the
    /// caller didn't request schema-driven streaming abort, or when the
    /// schema can't be canonicalized (logged + skipped so a malformed
    /// schema never silently degrades the whole call).
    pub(crate) fn from_payload(payload: &LlmRequestPayload) -> Option<Self> {
        if !payload.schema_stream_abort {
            return None;
        }
        let schema = payload.output_schema.as_ref()?;
        match StreamSchemaValidator::from_json_schema(schema) {
            Ok(validator) => Some(Self {
                validator,
                provider: payload.provider.clone(),
                model: payload.model.clone(),
                chunks_consumed: 0,
                fired: false,
            }),
            Err(err) => {
                crate::events::log_warn(
                    "llm",
                    &format!(
                        "schema_stream_abort: failed to canonicalize output_schema, \
                         continuing without mid-stream validation: {err}"
                    ),
                );
                None
            }
        }
    }

    /// Feed a visible text delta into the validator. Returns the abort
    /// info on the first chunk whose state transitions to `Invalid`;
    /// later chunks are ignored.
    pub(crate) fn observe(&mut self, delta: &str) -> Option<SchemaStreamAbort> {
        if self.fired || delta.is_empty() {
            return None;
        }
        self.chunks_consumed += 1;
        if let JsonStreamStatus::Invalid {
            reason_kind,
            reason,
            path,
        } = self.validator.feed(delta)
        {
            let abort = SchemaStreamAbort {
                provider: self.provider.clone(),
                model: self.model.clone(),
                reason_kind: *reason_kind,
                reason: reason.clone(),
                path: path.clone(),
                chunks_consumed: self.chunks_consumed,
            };
            self.fired = true;
            emit_agent_event(AgentTraceEvent::SchemaStreamAborted {
                provider: abort.provider.clone(),
                model: abort.model.clone(),
                reason_kind: abort.reason_kind.as_str().to_string(),
                reason: abort.reason.clone(),
                path: abort.path.clone(),
                chunks_consumed: abort.chunks_consumed,
            });
            if let Some(metrics) = crate::active_metrics_registry() {
                metrics.record_schema_stream_aborted(&abort.provider, &abort.model);
            }
            return Some(abort);
        }
        None
    }
}

impl SchemaStreamAbort {
    /// Convert the abort into the typed VM error the schema-retry loop catches.
    pub(crate) fn into_vm_error(self) -> VmError {
        VmError::SchemaStreamAbort(Box::new(self))
    }
}

/// Read a typed schema-stream abort without reconstructing it from display
/// text. The retry loop uses the exact validator fields carried by `VmError`.
pub(crate) fn parse_schema_stream_abort(err: &VmError) -> Option<SchemaStreamAbort> {
    err.schema_stream_abort().cloned()
}

/// Build the empty `LlmResult` stand-in used when the schema-retry loop
/// surfaces an abort to the caller after retries are exhausted. The
/// transcript event already records the abort metadata, but downstream
/// callers (e.g. `llm_call_safe`) still expect a dict envelope.
pub(crate) fn aborted_result_value(abort: &SchemaStreamAbort) -> VmValue {
    let mut meta = std::collections::BTreeMap::new();
    meta.put_str("reason_kind", abort.reason_kind.as_str());
    meta.put_str("reason", abort.reason.as_str());
    meta.put_str("path", abort.path.as_str());
    meta.insert(
        "chunks_consumed".to_string(),
        VmValue::Int(abort.chunks_consumed as i64),
    );
    meta.put_str("provider", abort.provider.as_str());
    meta.put_str("model", abort.model.as_str());
    let mut dict = std::collections::BTreeMap::new();
    dict.put_str("text", "");
    dict.put_str("model", abort.model.as_str());
    dict.put_str("provider", abort.provider.as_str());
    dict.insert("input_tokens".to_string(), VmValue::Int(0));
    dict.insert("output_tokens".to_string(), VmValue::Int(0));
    dict.insert("data".to_string(), VmValue::Nil);
    dict.insert("schema_stream_aborted".to_string(), VmValue::dict(meta));
    VmValue::dict(dict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::ErrorCategory;

    #[test]
    fn parses_round_trip_message() {
        let original = SchemaStreamAbort {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reason_kind: crate::value::SchemaValidationReasonKind::WrongType,
            reason: "expected type 'int', got JSON string".to_string(),
            path: "$.age".to_string(),
            chunks_consumed: 3,
        };
        let err = original.clone().into_vm_error();
        let parsed = parse_schema_stream_abort(&err).expect("parses");
        assert_eq!(parsed.provider, original.provider);
        assert_eq!(parsed.model, original.model);
        assert_eq!(parsed.reason, original.reason);
        assert_eq!(parsed.path, original.path);
        assert_eq!(parsed.chunks_consumed, original.chunks_consumed);
    }

    #[test]
    fn non_abort_error_is_none() {
        let err = VmError::CategorizedError {
            message: "something else".to_string(),
            category: ErrorCategory::Timeout,
        };
        assert!(parse_schema_stream_abort(&err).is_none());
    }
}
