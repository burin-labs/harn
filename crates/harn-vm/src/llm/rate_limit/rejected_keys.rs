//! Process-local memory of API keys a provider rejected.
//!
//! A provider that answers 401/403 for a key answers the same way for the next
//! request with that key. Without this memory every caller in the process (a
//! structured classification call, then the agent loop that follows it) paid
//! one more network round trip to learn what the first call already knew.
//!
//! The verdict is keyed by provider and a fingerprint of the key, never the
//! secret, so a different key is always sent. A rejection of the key itself
//! (`credential_scope: "key"`) covers every model; any other auth refusal, such
//! as a 403 for one model the key lacks, covers only that model, so a fallback
//! to a model the key can use is still sent. A verdict expires after
//! [`REJECTED_KEY_RETRY_AFTER`] so a key re-enabled on the provider side is
//! tried again, and a served call with the key clears its key-wide verdict.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::llm::api::{LlmErrorKind, LlmErrorReason};
use crate::llm::api::{CREDENTIAL_SCOPE_FIELD, CREDENTIAL_SCOPE_KEY};
use crate::value::{DictMap, VmDictExt, VmError, VmValue};

/// How long a provider's rejection of a key stands before the key is sent again.
pub(crate) const REJECTED_KEY_RETRY_AFTER: Duration = Duration::from_mins(5);

struct Rejection {
    rejected_at_ms: u128,
    fields: DictMap,
}

/// Provider, key fingerprint, and the model when the refusal named only it.
type RejectionKey = (String, String, Option<String>);

fn rejections() -> &'static Mutex<HashMap<RejectionKey, Rejection>> {
    static REJECTIONS: OnceLock<Mutex<HashMap<RejectionKey, Rejection>>> = OnceLock::new();
    REJECTIONS.get_or_init(Mutex::default)
}

fn rejection_key(provider: &str, key_id: &str, model: Option<&str>) -> RejectionKey {
    (
        provider.trim().to_ascii_lowercase(),
        key_id.to_string(),
        model.map(crate::llm_config::normalize_model_id),
    )
}

/// Fingerprint the key a call will present, or `None` when it presents none.
pub(super) fn key_id(api_key: &str) -> Option<String> {
    (!api_key.trim().is_empty()).then(|| crate::llm::rate_governor::org_key_id(api_key))
}

/// Refuse a call whose key this provider already rejected in this process,
/// for every model or for this one.
///
/// The refusal carries the provider's own classification (`auth`, terminal,
/// `auth_failure`), scope, and message, so every consumer that classifies a
/// real rejection classifies this one the same way. `attempt_count: 0` records
/// that no request was sent.
pub(super) fn refuse_rejected_key(
    provider: &str,
    model: &str,
    key_id: Option<&str>,
) -> Result<(), VmError> {
    let Some(key_id) = key_id else {
        return Ok(());
    };
    let now_ms = crate::clock_mock::instant_now().as_millis();
    let retry_after_ms = REJECTED_KEY_RETRY_AFTER.as_millis();
    let mut rejections = rejections().lock().unwrap_or_else(|e| e.into_inner());
    for key in [
        rejection_key(provider, key_id, None),
        rejection_key(provider, key_id, Some(model)),
    ] {
        let Some(rejection) = rejections.get(&key) else {
            continue;
        };
        let elapsed_ms = now_ms.saturating_sub(rejection.rejected_at_ms);
        if elapsed_ms >= retry_after_ms {
            rejections.remove(&key);
            continue;
        }
        let remaining_s = (retry_after_ms - elapsed_ms).div_ceil(1000);
        let target = if key.2.is_some() {
            format!("this API key for '{model}'")
        } else {
            "this API key".to_string()
        };
        let mut fields = rejection.fields.clone();
        let original = fields
            .get("message")
            .map(VmValue::display)
            .unwrap_or_default();
        fields.put_str(
            "message",
            format!(
                "{original} [not sent: {provider} already rejected {target} in this process; \
                 it is sent again in {remaining_s}s]"
            ),
        );
        fields.put_int("attempt_count", 0);
        return Err(VmError::Thrown(VmValue::dict(fields)));
    }
    Ok(())
}

/// Record what the provider said about a key after a dispatched call.
pub(super) fn observe_key_outcome<T>(
    provider: &str,
    model: &str,
    key_id: Option<&str>,
    outcome: &Result<T, VmError>,
) {
    let Some(key_id) = key_id else {
        return;
    };
    let error = match outcome {
        Ok(_) => {
            let mut rejections = rejections().lock().unwrap_or_else(|e| e.into_inner());
            rejections.remove(&rejection_key(provider, key_id, None));
            rejections.remove(&rejection_key(provider, key_id, Some(model)));
            return;
        }
        Err(error) => error,
    };
    let classified = crate::llm::api::classify_vm_llm_error(error);
    if classified.reason != LlmErrorReason::AuthFailure || classified.kind != LlmErrorKind::Terminal
    {
        return;
    }
    let original = match error {
        VmError::Thrown(VmValue::Dict(fields)) => Some(fields),
        _ => None,
    };
    let scope = original.and_then(|fields| fields.get(CREDENTIAL_SCOPE_FIELD).cloned());
    let key_scoped = scope
        .as_ref()
        .is_some_and(|scope| scope.display() == CREDENTIAL_SCOPE_KEY);
    // Keep only the classification and message; per-response details such as
    // quota snapshots and Retry-After belong to the response that sent them.
    let message = original
        .and_then(|fields| fields.get("message"))
        .map(VmValue::display)
        .unwrap_or_else(|| error.to_string());
    let mut fields = DictMap::new();
    fields.put_opt(CREDENTIAL_SCOPE_FIELD, scope);
    fields.put_str("category", crate::value::ErrorCategory::Auth.as_str());
    fields.put_str("kind", classified.kind.as_str());
    fields.put_str("reason", classified.reason.as_str());
    fields.put_str("message", message);
    let key = rejection_key(provider, key_id, (!key_scoped).then_some(model));
    rejections()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            key,
            Rejection {
                rejected_at_ms: crate::clock_mock::instant_now().as_millis(),
                fields,
            },
        );
}

pub(super) fn reset() {
    rejections()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}
