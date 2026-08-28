//! Native-platform normalization for exact replay result evidence.

use std::fs;
use std::path::Path;

use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};

const VOLATILE_FIELDS: &[&str] = &[
    "audit_id",
    "command_id",
    "duration_ms",
    "ended_at",
    "handle_id",
    "output_path",
    "pid",
    "process_group_id",
    "sandbox",
    "started_at",
    "stderr_path",
    "stdout_path",
];

pub(super) fn canonical_replay_result(value: &JsonValue, workspace: &Path) -> JsonValue {
    let lexical_root = workspace.to_string_lossy().replace('\\', "/");
    let canonical_root = fs::canonicalize(workspace)
        .ok()
        .map(|path| path.to_string_lossy().replace('\\', "/"));
    normalize(value, &lexical_root, canonical_root.as_deref())
}

fn normalize(value: &JsonValue, lexical_root: &str, canonical_root: Option<&str>) -> JsonValue {
    match value {
        JsonValue::Object(object) => {
            let mut normalized = object
                .iter()
                .filter(|(key, _)| !VOLATILE_FIELDS.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), normalize(value, lexical_root, canonical_root)))
                .collect::<serde_json::Map<_, _>>();
            normalize_process_output(object, &mut normalized);
            JsonValue::Object(normalized)
        }
        JsonValue::Array(values) => JsonValue::Array(
            values
                .iter()
                .map(|value| normalize(value, lexical_root, canonical_root))
                .collect(),
        ),
        JsonValue::String(text) => {
            let slash_text = text.replace('\\', "/");
            let normalized = canonical_root
                .and_then(|root| slash_text.strip_prefix(root))
                .or_else(|| slash_text.strip_prefix(lexical_root))
                .map_or_else(|| text.clone(), |suffix| format!("$WORKSPACE{suffix}"));
            JsonValue::String(normalized)
        }
        _ => value.clone(),
    }
}

fn normalize_process_output(
    original: &serde_json::Map<String, JsonValue>,
    normalized: &mut serde_json::Map<String, JsonValue>,
) {
    if !normalized.contains_key("stdout") && !normalized.contains_key("stderr") {
        return;
    }

    let captured_bytes =
        stream_len(original, "stdout").saturating_add(stream_len(original, "stderr"));
    let capture_is_complete = original
        .get("byte_count")
        .and_then(JsonValue::as_u64)
        .is_some_and(|byte_count| byte_count == captured_bytes as u64);
    let stdout = normalized_stream(normalized, "stdout");
    let stderr = normalized_stream(normalized, "stderr");
    if normalized.contains_key("stdout") {
        normalized.insert("stdout".to_string(), JsonValue::String(stdout.clone()));
    }
    if normalized.contains_key("stderr") {
        normalized.insert("stderr".to_string(), JsonValue::String(stderr.clone()));
    }

    let combined = stdout + &stderr;
    if capture_is_complete && normalized.contains_key("byte_count") {
        normalized.insert("byte_count".to_string(), json!(combined.len()));
    }
    if capture_is_complete && normalized.contains_key("line_count") {
        normalized.insert("line_count".to_string(), json!(combined.lines().count()));
    }
    if capture_is_complete && normalized.contains_key("output_sha256") {
        normalized.insert(
            "output_sha256".to_string(),
            JsonValue::String(format!(
                "sha256:{}",
                hex::encode(Sha256::digest(combined.as_bytes())),
            )),
        );
    }
}

fn stream_len(object: &serde_json::Map<String, JsonValue>, field: &str) -> usize {
    object
        .get(field)
        .and_then(JsonValue::as_str)
        .map_or(0, str::len)
}

fn normalized_stream(object: &serde_json::Map<String, JsonValue>, field: &str) -> String {
    object
        .get(field)
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .replace("\r\n", "\n")
}

#[cfg(test)]
mod tests {
    use super::canonical_replay_result;

    #[test]
    fn missing_byte_count_does_not_rewrite_derived_capture_evidence() {
        let workspace = tempfile::tempdir().expect("workspace");
        let result = canonical_replay_result(
            &serde_json::json!({
                "stdout": "ok\r\n",
                "line_count": 7,
                "output_sha256": "sha256:unverified"
            }),
            workspace.path(),
        );

        assert_eq!(result["stdout"], "ok\n");
        assert_eq!(result["line_count"], 7);
        assert_eq!(result["output_sha256"], "sha256:unverified");
    }
}
