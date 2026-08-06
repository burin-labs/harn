//! Stable content digests for retained LLM context.
//!
//! Hashing happens after the active redaction policy so transcript payloads,
//! assembly manifests, and served-context receipts all name the same retained
//! bytes. Keep these helpers as the only owner of that surface.

pub(crate) fn stable_redacted_string_hash(value: &str) -> String {
    let policy = crate::redact::current_policy();
    let redacted = match policy.redact_json(&serde_json::Value::String(value.to_string())) {
        serde_json::Value::String(redacted) => redacted,
        other => serde_json::to_string(&other).unwrap_or_default(),
    };
    stable_content_hash(redacted.as_bytes())
}

pub(crate) fn stable_redacted_json_hash(value: &serde_json::Value) -> String {
    let redacted = crate::redact::current_policy().redact_json(value);
    let encoded = serde_json::to_vec(&redacted).unwrap_or_default();
    stable_content_hash(&encoded)
}

fn stable_content_hash(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}
