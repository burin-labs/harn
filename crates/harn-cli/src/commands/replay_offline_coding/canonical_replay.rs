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

// The owning cleanup report already makes survivor_count null unless a census
// or guardian proves it. Preserve that outcome instead of hashing the host's
// chosen proof mechanism or unrelated process population.
const CLEANUP_VOLATILE_FIELDS: &[&str] = &[
    "root_pid",
    "parent_pid",
    "census_complete",
    "census_control_read",
    "census_scanned_processes",
    "census_unreadable_processes",
    "guardian_custody_complete",
    "attempted_signals",
    "signals",
];

pub(super) fn canonical_replay_result(value: &JsonValue, workspace: &Path) -> JsonValue {
    let lexical_root = workspace.to_string_lossy().replace('\\', "/");
    let canonical_root = fs::canonicalize(workspace)
        .ok()
        .map(|path| path.to_string_lossy().replace('\\', "/"));
    normalize(value, &lexical_root, canonical_root.as_deref(), false)
}

fn normalize(
    value: &JsonValue,
    lexical_root: &str,
    canonical_root: Option<&str>,
    process_cleanup: bool,
) -> JsonValue {
    match value {
        JsonValue::Object(object) => {
            let mut normalized = object
                .iter()
                .filter(|(key, _)| !VOLATILE_FIELDS.contains(&key.as_str()))
                .filter(|(key, _)| {
                    !process_cleanup || !CLEANUP_VOLATILE_FIELDS.contains(&key.as_str())
                })
                .map(|(key, value)| {
                    (
                        key.clone(),
                        normalize(
                            value,
                            lexical_root,
                            canonical_root,
                            process_cleanup || key == "process_cleanup",
                        ),
                    )
                })
                .collect::<serde_json::Map<_, _>>();
            normalize_process_output(object, &mut normalized);
            JsonValue::Object(normalized)
        }
        JsonValue::Array(values) => JsonValue::Array(
            values
                .iter()
                .map(|value| normalize(value, lexical_root, canonical_root, process_cleanup))
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
    fn cleanup_identity_normalization_preserves_decision_evidence() {
        let workspace = tempfile::tempdir().unwrap();
        let first = serde_json::json!({"process_cleanup": {
            "attempted_signals": ["SIGTERM"],
            "root_pid": 10, "census_scanned_processes": 100,
            "census_unreadable_processes": 0, "census_complete": true,
            "guardian_custody_complete": false,
            "survivor_count": 0, "omitted_child_count": 0,
            "observed_children": [{"pid": 11, "parent_pid": 10,
                "depth": 1, "command_name": "sleep", "signals": ["SIGTERM"], "alive_after_cleanup": false}]
        }});
        let mut second = first.clone();
        second["process_cleanup"]["root_pid"] = 20.into();
        second["process_cleanup"]["census_scanned_processes"] = 200.into();
        second["process_cleanup"]["census_unreadable_processes"] = 50.into();
        second["process_cleanup"]["census_complete"] = false.into();
        second["process_cleanup"]["guardian_custody_complete"] = true.into();
        second["process_cleanup"]["attempted_signals"] = serde_json::json!(["SIGKILL"]);
        second["process_cleanup"]["observed_children"][0]["signals"] =
            serde_json::json!(["SIGKILL"]);
        second["process_cleanup"]["observed_children"][0]["pid"] = 21.into();
        second["process_cleanup"]["observed_children"][0]["parent_pid"] = 20.into();
        let expected = canonical_replay_result(&first, workspace.path());
        assert_eq!(expected, canonical_replay_result(&second, workspace.path()));

        for (field, value) in [
            ("survivor_count", serde_json::json!(1)),
            ("omitted_child_count", serde_json::json!(1)),
        ] {
            let mut changed = second.clone();
            changed["process_cleanup"][field] = value;
            assert_ne!(
                expected,
                canonical_replay_result(&changed, workspace.path()),
                "{field}"
            );
        }
        let mut unknown = second.clone();
        unknown["process_cleanup"]["guardian_custody_complete"] = false.into();
        unknown["process_cleanup"]["survivor_count"] = serde_json::Value::Null;
        let unknown_result = canonical_replay_result(&unknown, workspace.path());
        assert_ne!(expected, unknown_result);
        unknown["process_cleanup"]
            .as_object_mut()
            .unwrap()
            .remove("guardian_custody_complete");
        assert_eq!(
            unknown_result,
            canonical_replay_result(&unknown, workspace.path())
        );
        second["process_cleanup"]["observed_children"][0]["alive_after_cleanup"] = true.into();
        assert_ne!(expected, canonical_replay_result(&second, workspace.path()));
        assert_ne!(
            expected,
            canonical_replay_result(&serde_json::json!({}), workspace.path())
        );
        assert_ne!(
            expected,
            canonical_replay_result(
                &serde_json::json!({"process_cleanup": null}),
                workspace.path()
            )
        );
    }

    #[test]
    fn absent_null_and_unknown_cleanup_are_distinct() {
        let workspace = tempfile::tempdir().unwrap();
        let outcomes = [
            serde_json::json!({}),
            serde_json::json!({"process_cleanup": null}),
            serde_json::json!({"process_cleanup": {"survivor_count": null}}),
            serde_json::json!({"process_cleanup": {"survivor_count": 0}}),
        ];
        for (index, outcome) in outcomes.iter().enumerate() {
            for other in &outcomes[index + 1..] {
                assert_ne!(
                    canonical_replay_result(outcome, workspace.path()),
                    canonical_replay_result(other, workspace.path()),
                );
            }
        }
    }

    #[test]
    fn cleanup_metadata_names_remain_data_outside_cleanup_receipts() {
        let workspace = tempfile::tempdir().unwrap();
        let value = serde_json::json!({"root_pid": 10, "parent_pid": 9, "census_scanned_processes": 100,
                "signal": "SIGTERM", "signals": ["SIGTERM"], "attempted_signals": ["SIGTERM"]});
        assert_eq!(value, canonical_replay_result(&value, workspace.path()));
    }

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
