//! Process-local memory of API keys a provider rejected.
//!
//! A provider that answers 401/403 for a key answers the same way for the next
//! request with that key. Without this memory every caller in the process (a
//! structured classification call, then the agent loop that follows it) paid
//! one more network round trip to learn what the first call already knew.
//!
//! The verdict is keyed by provider and a fingerprint of the key, never the
//! secret, so a different key is always sent. It expires after
//! [`REJECTED_KEY_RETRY_AFTER`] so a key re-enabled on the provider side is
//! tried again, and a served call with the key clears it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::llm::api::{LlmErrorKind, LlmErrorReason};
use crate::value::{VmError, VmValue};

/// How long a provider's rejection of a key stands before the key is sent again.
pub(crate) const REJECTED_KEY_RETRY_AFTER: Duration = Duration::from_mins(5);

struct Rejection {
    rejected_at_ms: u128,
    message: String,
}

type RejectionKey = (String, String);

fn rejections() -> &'static Mutex<HashMap<RejectionKey, Rejection>> {
    static REJECTIONS: OnceLock<Mutex<HashMap<RejectionKey, Rejection>>> = OnceLock::new();
    REJECTIONS.get_or_init(Mutex::default)
}

fn rejection_key(provider: &str, key_id: &str) -> RejectionKey {
    (provider.trim().to_ascii_lowercase(), key_id.to_string())
}

/// Fingerprint the key a call will present, or `None` when it presents none.
pub(super) fn key_id(api_key: &str) -> Option<String> {
    (!api_key.trim().is_empty()).then(|| crate::llm::rate_governor::org_key_id(api_key))
}

/// Refuse a call whose key this provider already rejected in this process.
///
/// The refusal carries the provider's own classification (`auth`, terminal,
/// `auth_failure`) and message, so every consumer that classifies a real
/// rejection classifies this one the same way. `attempt_count: 0` records that
/// no request was sent.
pub(super) fn refuse_rejected_key(provider: &str, key_id: Option<&str>) -> Result<(), VmError> {
    let Some(key_id) = key_id else {
        return Ok(());
    };
    let key = rejection_key(provider, key_id);
    let now_ms = crate::clock_mock::instant_now().as_millis();
    let mut rejections = rejections().lock().unwrap_or_else(|e| e.into_inner());
    let Some(rejection) = rejections.get(&key) else {
        return Ok(());
    };
    let elapsed_ms = now_ms.saturating_sub(rejection.rejected_at_ms);
    let retry_after_ms = REJECTED_KEY_RETRY_AFTER.as_millis();
    if elapsed_ms >= retry_after_ms {
        rejections.remove(&key);
        return Ok(());
    }
    let remaining_s = (retry_after_ms - elapsed_ms).div_ceil(1000);
    let message = format!(
        "{} [not sent: {provider} already rejected this API key in this process; \
         the key is sent again in {remaining_s}s]",
        rejection.message
    );
    Err(VmError::Thrown(VmValue::dict(BTreeMap::from([
        (
            "category".to_string(),
            VmValue::string(crate::value::ErrorCategory::Auth.as_str()),
        ),
        (
            "kind".to_string(),
            VmValue::string(LlmErrorKind::Terminal.as_str()),
        ),
        (
            "reason".to_string(),
            VmValue::string(LlmErrorReason::AuthFailure.as_str()),
        ),
        ("attempt_count".to_string(), VmValue::Int(0)),
        ("message".to_string(), VmValue::string(message)),
    ]))))
}

/// Record what the provider said about a key after a dispatched call.
pub(super) fn observe_key_outcome<T>(
    provider: &str,
    key_id: Option<&str>,
    outcome: &Result<T, VmError>,
) {
    let Some(key_id) = key_id else {
        return;
    };
    let key = rejection_key(provider, key_id);
    match outcome {
        Ok(_) => {
            rejections()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
        }
        Err(error) => {
            let classified = crate::llm::api::classify_vm_llm_error(error);
            if classified.reason != LlmErrorReason::AuthFailure
                || classified.kind != LlmErrorKind::Terminal
            {
                return;
            }
            let message = match error {
                VmError::Thrown(VmValue::Dict(fields)) => fields
                    .get("message")
                    .map(VmValue::display)
                    .unwrap_or_else(|| error.to_string()),
                _ => error.to_string(),
            };
            rejections()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    key,
                    Rejection {
                        rejected_at_ms: crate::clock_mock::instant_now().as_millis(),
                        message,
                    },
                );
        }
    }
}

pub(super) fn reset() {
    rejections()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}
