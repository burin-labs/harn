//! The recorder owns privacy before either metadata or blob bytes reach storage.

use serde::{de::DeserializeOwned, Serialize};

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

pub(super) fn payload(bytes: Vec<u8>) -> (Vec<u8>, bool) {
    let policy = crate::redact::current_policy();
    if let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
        let original = value.clone();
        policy.redact_json_in_place(&mut value);
        return if value == original {
            (bytes, false)
        } else {
            (crate::canonical_json::to_vec(&value), true)
        };
    }
    // Binary effects can still contain credentials. Preserve unmodified bytes;
    // a sanitized binary becomes inspectable text and refuses exact replay.
    let text = String::from_utf8_lossy(&bytes);
    match policy.redact_string(&text) {
        std::borrow::Cow::Owned(redacted) => (redacted.into_bytes(), true),
        std::borrow::Cow::Borrowed(_) => (bytes, false),
    }
}
