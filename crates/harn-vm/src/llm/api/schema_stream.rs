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
use crate::value::{ErrorCategory, VmError, VmValue};

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

/// Information surfaced when the streaming abort fires. The transport
/// converts this into a `VmError::CategorizedError` via [`Self::into_vm_error`]
/// so the schema-retry loop can pattern-match on category.
#[derive(Clone, Debug)]
pub(crate) struct SchemaStreamAbort {
    pub provider: String,
    pub model: String,
    pub reason: String,
    pub path: String,
    pub chunks_consumed: usize,
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
        if let JsonStreamStatus::Invalid { reason, path } = self.validator.feed(delta) {
            let abort = SchemaStreamAbort {
                provider: self.provider.clone(),
                model: self.model.clone(),
                reason: reason.clone(),
                path: path.clone(),
                chunks_consumed: self.chunks_consumed,
            };
            self.fired = true;
            emit_agent_event(AgentTraceEvent::SchemaStreamAborted {
                provider: abort.provider.clone(),
                model: abort.model.clone(),
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
    /// Convert the abort into the categorized VmError the schema-retry
    /// loop catches. The message is human-readable; structured fields
    /// (provider, model, path, reason, chunks_consumed) are recovered by
    /// re-parsing in [`Self::from_vm_error`].
    pub(crate) fn into_vm_error(self) -> VmError {
        VmError::CategorizedError {
            message: format!(
                "schema_stream_aborted at {path}: {reason} \
                 (provider={provider} model={model} chunks_consumed={chunks})",
                path = self.path,
                reason = self.reason,
                provider = self.provider,
                model = self.model,
                chunks = self.chunks_consumed,
            ),
            category: ErrorCategory::SchemaStreamAborted,
        }
    }
}

/// Recognize a `SchemaStreamAborted` error and rebuild the structured
/// fields from its message. Returns `None` for any other error. Lets the
/// schema-retry loop drive its corrective nudge from the same `path` /
/// `reason` the transport saw, without piping a second sidecar value
/// through every retry plumb.
pub(crate) fn parse_schema_stream_abort(err: &VmError) -> Option<SchemaStreamAbort> {
    let VmError::CategorizedError { message, category } = err else {
        return None;
    };
    if !matches!(category, ErrorCategory::SchemaStreamAborted) {
        return None;
    }
    parse_abort_message(message)
}

fn parse_abort_message(message: &str) -> Option<SchemaStreamAbort> {
    // Format: "schema_stream_aborted at <path>: <reason> (provider=... model=... chunks_consumed=...)"
    let body = message.strip_prefix("schema_stream_aborted at ")?;
    let (path_part, rest) = body.split_once(": ")?;
    let (reason_part, meta_part) = rest.rsplit_once(" (provider=")?;
    let meta = meta_part.trim_end_matches(')');
    let mut provider = String::new();
    let mut model = String::new();
    let mut chunks_consumed: usize = 0;
    for entry in meta.split_whitespace() {
        if let Some(value) = entry.strip_prefix("model=") {
            model = value.to_string();
        } else if let Some(value) = entry.strip_prefix("chunks_consumed=") {
            chunks_consumed = value.parse().unwrap_or(0);
        } else {
            // Leading "provider=..." chunk; `split_once` left the value
            // here without its key prefix.
            provider = entry.to_string();
        }
    }
    Some(SchemaStreamAbort {
        provider,
        model,
        reason: reason_part.to_string(),
        path: path_part.to_string(),
        chunks_consumed,
    })
}

/// Build the empty `LlmResult` stand-in used when the schema-retry loop
/// surfaces an abort to the caller after retries are exhausted. The
/// transcript event already records the abort metadata, but downstream
/// callers (e.g. `llm_call_safe`) still expect a dict envelope.
pub(crate) fn aborted_result_value(abort: &SchemaStreamAbort) -> VmValue {
    let mut meta = std::collections::BTreeMap::new();
    meta.insert(
        "reason".to_string(),
        VmValue::String(std::sync::Arc::from(abort.reason.as_str())),
    );
    meta.insert(
        "path".to_string(),
        VmValue::String(std::sync::Arc::from(abort.path.as_str())),
    );
    meta.insert(
        "chunks_consumed".to_string(),
        VmValue::Int(abort.chunks_consumed as i64),
    );
    meta.insert(
        "provider".to_string(),
        VmValue::String(std::sync::Arc::from(abort.provider.as_str())),
    );
    meta.insert(
        "model".to_string(),
        VmValue::String(std::sync::Arc::from(abort.model.as_str())),
    );
    let mut dict = std::collections::BTreeMap::new();
    dict.insert(
        "text".to_string(),
        VmValue::String(std::sync::Arc::from("")),
    );
    dict.insert(
        "model".to_string(),
        VmValue::String(std::sync::Arc::from(abort.model.as_str())),
    );
    dict.insert(
        "provider".to_string(),
        VmValue::String(std::sync::Arc::from(abort.provider.as_str())),
    );
    dict.insert("input_tokens".to_string(), VmValue::Int(0));
    dict.insert("output_tokens".to_string(), VmValue::Int(0));
    dict.insert("data".to_string(), VmValue::Nil);
    dict.insert(
        "schema_stream_aborted".to_string(),
        VmValue::Dict(std::sync::Arc::new(meta)),
    );
    VmValue::Dict(std::sync::Arc::new(dict))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_round_trip_message() {
        let original = SchemaStreamAbort {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
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
