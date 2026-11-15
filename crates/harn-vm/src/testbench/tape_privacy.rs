//! The recorder owns privacy before either metadata or blob bytes reach storage.

use serde::{de::DeserializeOwned, Serialize};

use super::RedactionReplay;

pub(crate) fn replay_digest(value: &serde_json::Value) -> String {
    let sanitized = crate::redact::current_policy().redact_json(value);
    super::content_hash(&crate::canonical_json::to_vec(&sanitized))
}

pub(super) fn metadata<T: Serialize + DeserializeOwned>(value: T) -> Result<(T, bool), String> {
    let mut sanitized = serde_json::to_value(&value)
        .map_err(|_| "could not serialize tape metadata".to_string())?;
    let original = sanitized.clone();
    crate::redact::current_policy().redact_json_in_place(&mut sanitized);
    if sanitized == original {
        return Ok((value, false));
    }
    serde_json::from_value(sanitized)
        .map(|value| (value, true))
        .map_err(|_| "redacted tape metadata no longer fits its typed contract".to_string())
}

pub(super) fn payload(bytes: Vec<u8>) -> (Vec<u8>, Option<RedactionReplay>) {
    let policy = crate::redact::current_policy();
    if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
        let original = value.clone();
        policy.redact_json_in_place(&mut value);
        return if value == original {
            (bytes, None)
        } else {
            (
                crate::canonical_json::to_vec(&value),
                Some(RedactionReplay::StablePlaceholder),
            )
        };
    }
    // Binary effects can still contain credentials. Preserve unmodified bytes;
    // a sanitized binary becomes inspectable text and refuses exact replay.
    let text = String::from_utf8_lossy(&bytes);
    match policy.redact_string(&text) {
        std::borrow::Cow::Owned(redacted) => {
            let replay = if std::str::from_utf8(&bytes).is_ok() {
                RedactionReplay::StablePlaceholder
            } else {
                RedactionReplay::LossyBinary
            };
            (redacted.into_bytes(), Some(replay))
        }
        std::borrow::Cow::Borrowed(_) => (bytes, None),
    }
}
