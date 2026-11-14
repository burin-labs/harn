//! Opt-in raw provider request and response receipts.

use std::future::Future;
use std::path::PathBuf;

use crate::value::{DictMap, VmDictExt, VmError, VmValue};

use super::{append_llm_transcript_entry_to_dir, chrono_now, current_transcript_dir};

tokio::task_local! {
    static RAW_PROVIDER_CAPTURE_CONTEXT: RawProviderCaptureContext;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawProviderCaptureContext {
    pub(crate) call_id: String,
    pub(crate) iteration: usize,
    transcript_dir: Option<String>,
    enabled: bool,
}

impl RawProviderCaptureContext {
    pub(super) fn new(call_id: String, iteration: usize, enabled: bool) -> Self {
        Self {
            call_id,
            iteration,
            transcript_dir: current_transcript_dir(),
            enabled,
        }
    }
}

pub(crate) async fn with_raw_provider_capture_context<F>(
    context: RawProviderCaptureContext,
    future: F,
) -> F::Output
where
    F: Future,
{
    RAW_PROVIDER_CAPTURE_CONTEXT.scope(context, future).await
}

pub(crate) fn current_raw_provider_capture_context() -> Option<RawProviderCaptureContext> {
    RAW_PROVIDER_CAPTURE_CONTEXT.try_with(Clone::clone).ok()
}

pub(crate) fn validate_raw_provider_capture_destination(
    raw: Option<bool>,
    selected_dir: Option<&str>,
) -> Result<(), VmError> {
    if raw == Some(true)
        && selected_dir.is_none_or(|dir| dir.trim().is_empty())
        && current_transcript_dir().is_none_or(|dir| dir.trim().is_empty())
    {
        let mut error = DictMap::new();
        error.put_str("category", "configuration");
        error.put_str("reason_code", "llm_transcript_directory_required");
        error.put_str(
            "message",
            "llm_transcript_raw=true requires llm_transcript_dir or an active transcript directory",
        );
        return Err(VmError::Thrown(VmValue::dict(error)));
    }
    Ok(())
}

pub(crate) fn raw_provider_capture_enabled(context: Option<&RawProviderCaptureContext>) -> bool {
    context.is_some_and(|context| context.enabled && context.transcript_dir.is_some())
}

pub(crate) fn persist_raw_provider_request(
    context: Option<&RawProviderCaptureContext>,
    provider: &str,
    model: &str,
    wire_dialect: &str,
    attempt: Option<usize>,
    body: &serde_json::Value,
) -> Option<String> {
    let context = context?;
    if !raw_provider_capture_enabled(Some(context)) {
        return None;
    }
    let envelope = serde_json::json!({
        "schema_version": "harn.llm.raw_provider_request.v1",
        "kind": "request",
        "captured_at": chrono_now(),
        "call_id": context.call_id,
        "iteration": context.iteration,
        "attempt": attempt,
        "provider": provider,
        "model": model,
        "wire_dialect": wire_dialect,
        "body": body,
    });
    write_raw_provider_sidecar(context, "request", provider, model, attempt, envelope)
}

pub(crate) fn persist_raw_provider_response(
    context: Option<&RawProviderCaptureContext>,
    provider: &str,
    model: &str,
    transport: &str,
    attempt: Option<usize>,
    status: u16,
    content_type: Option<&str>,
    body_text: &str,
) -> Option<String> {
    let context = context?;
    if !raw_provider_capture_enabled(Some(context)) {
        return None;
    }
    let parsed_json = serde_json::from_str::<serde_json::Value>(body_text).ok();
    let envelope = serde_json::json!({
        "schema_version": "harn.llm.raw_provider_response.v1",
        "kind": "response",
        "captured_at": chrono_now(),
        "call_id": context.call_id,
        "iteration": context.iteration,
        "attempt": attempt,
        "provider": provider,
        "model": model,
        "transport": transport,
        "status": status,
        "content_type": content_type,
        "body_text": body_text,
        "body_json": parsed_json,
    });
    write_raw_provider_sidecar(
        context,
        &format!("response-{transport}"),
        provider,
        model,
        attempt,
        envelope,
    )
}

/// Safe response-envelope facts retained when reqwest fails while draining a
/// non-streaming provider body. Body bytes are deliberately absent because
/// reqwest did not yield a complete body.
pub(crate) struct RawProviderResponseFailureCapture<'a> {
    pub(crate) transport: &'a str,
    pub(crate) attempt: Option<usize>,
    pub(crate) status: u16,
    pub(crate) content_type: Option<&'a str>,
    pub(crate) request_id: Option<&'a str>,
    pub(crate) generation_id: Option<&'a str>,
    pub(crate) error: &'a VmError,
}

pub(crate) fn persist_raw_provider_response_failure(
    context: Option<&RawProviderCaptureContext>,
    provider: &str,
    model: &str,
    capture: RawProviderResponseFailureCapture<'_>,
) -> Option<String> {
    let context = context?;
    if !raw_provider_capture_enabled(Some(context)) {
        return None;
    }
    let category = crate::value::error_to_category(capture.error);
    let message = crate::egress::redact_diagnostic_text(&capture.error.to_string());
    let classified = super::api::classify_llm_error(category.clone(), &message);
    let envelope = serde_json::json!({
        "schema_version": "harn.llm.raw_provider_response_failure.v1",
        "kind": "response_failure",
        "captured_at": chrono_now(),
        "call_id": context.call_id,
        "iteration": context.iteration,
        "attempt": capture.attempt,
        "provider": provider,
        "model": model,
        "transport": capture.transport,
        "status": capture.status,
        "content_type": capture.content_type,
        "request_id": capture.request_id,
        "generation_id": capture.generation_id,
        "failure": {
            "category": category.as_str(),
            "kind": classified.kind.as_str(),
            "reason": classified.reason.as_str(),
            "retryable": classified.kind == super::api::LlmErrorKind::Transient,
            "message": message,
        },
    });
    write_raw_provider_sidecar(
        context,
        &format!("response-{}-failure", capture.transport),
        provider,
        model,
        capture.attempt,
        envelope,
    )
}

fn write_raw_provider_sidecar(
    context: &RawProviderCaptureContext,
    suffix: &str,
    provider: &str,
    model: &str,
    attempt: Option<usize>,
    mut envelope: serde_json::Value,
) -> Option<String> {
    crate::redact::current_policy().redact_json_in_place(&mut envelope);
    let dir = context.transcript_dir.as_deref()?;
    let raw_dir = PathBuf::from(&dir).join("raw-provider");
    let call_id = raw_provider_file_id(&context.call_id);
    let attempt_part = attempt
        .map(|attempt| format!("-attempt-{attempt}"))
        .unwrap_or_default();
    let filename = format!("{call_id}{attempt_part}-{suffix}.json");
    let relative_path = format!("raw-provider/{filename}");
    let path = raw_dir.join(filename);
    static RAW_PROVIDER_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let written = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&raw_dir)?;
        let encoded = serde_json::to_vec_pretty(&envelope).map_err(std::io::Error::other)?;
        let _guard = RAW_PROVIDER_WRITE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;
        use std::io::Write;
        file.write_all(&encoded)?;
        file.write_all(b"\n")
    })();
    if let Err(error) = written {
        report_raw_capture_failure(
            context,
            suffix,
            "sidecar_write_failed",
            &error.kind().to_string(),
        );
        return None;
    }
    let pointer_written = append_llm_transcript_entry_to_dir(
        &serde_json::json!({
            "type": "provider_raw_capture",
            "timestamp": chrono_now(),
            "span_id": crate::tracing::current_span_id(),
            "call_id": context.call_id,
            "iteration": context.iteration,
            "attempt": attempt,
            "provider": provider,
            "model": model,
            "capture": suffix,
            "path": relative_path,
        }),
        Some(dir),
    );
    if pointer_written {
        Some(relative_path)
    } else {
        report_raw_capture_failure(
            context,
            suffix,
            "transcript_pointer_write_failed",
            "transcript append failed",
        );
        None
    }
}

fn report_raw_capture_failure(
    context: &RawProviderCaptureContext,
    capture: &str,
    reason: &str,
    detail: &str,
) {
    let message = crate::egress::redact_diagnostic_text(&format!(
        "raw provider capture failed: {reason} ({detail}), call {}",
        context.call_id,
    ));
    eprintln!("[harn] warning [provider_raw_capture_failed]: {message}");
    append_llm_transcript_entry_to_dir(
        &serde_json::json!({
            "type": "provider_raw_capture_failed",
            "timestamp": chrono_now(),
            "call_id": context.call_id,
            "iteration": context.iteration,
            "capture": capture,
            "reason_code": reason,
            "message": message,
        }),
        context.transcript_dir.as_deref(),
    );
}

fn raw_provider_file_id(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "call".to_string()
    } else {
        sanitized
    }
}
