//! Manifest-producing redaction for whole transcript/record structures.
//!
//! [`super::RedactionPolicy`] scrubs leaf strings, headers, URLs, and
//! JSON fields. This submodule adds the *export/share* layer on top:
//! a single canonical walk that redacts an arbitrary JSON structure —
//! a transcript, a run record, a session bundle — while recording an
//! auditable [`RedactionEntry`] for every value it touched, plus the
//! symmetric [`RedactionPolicy::find_unredacted_secret`] gate that a
//! share/ingest boundary uses to refuse a payload that still carries a
//! high-confidence secret.
//!
//! These were previously private helpers inside `session_bundle`. They
//! live here so every downstream host that exports a transcript (portal
//! Markdown/JSON download, TUI export, harn-cloud tape ingest) calls
//! one engine instead of reimplementing the walk and drifting from the
//! leaf-scrubbing policy.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::{RedactionPolicy, REDACTED_PLACEHOLDER};

/// One line in a redaction manifest: the JSON path that was touched,
/// the reason it was redacted, the action taken, and the replacement
/// that now sits at that path. Consumers use it to show "what did we
/// scrub before sharing this?" and to attribute a leak to a provider
/// via the `<redacted:<pattern>:<len>>` replacement string.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RedactionEntry {
    pub path: String,
    pub class: String,
    pub action: String,
    pub replacement: Option<String>,
}

/// A high-confidence secret that survived redaction, located by
/// [`RedactionPolicy::find_unredacted_secret`]. `path` is the JSON path
/// to the offending string; `excerpt` is a bounded, non-sensitive
/// prefix suitable for an error message (it is the leading characters
/// of the value, so callers must not log it verbatim into a durable
/// sink without their own redaction).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnredactedSecret {
    pub path: String,
    pub excerpt: String,
}

impl RedactionPolicy {
    /// Redact an arbitrary JSON structure in place and return an
    /// auditable manifest of every value that changed.
    ///
    /// This is the canonical whole-transcript / whole-record entry
    /// point: it recursively walks objects and arrays, replaces
    /// sensitive-named fields wholesale, and scans every leaf string
    /// for secret patterns and credentialed URLs via
    /// [`RedactionPolicy::redact_string`]. Message bodies, tool inputs,
    /// and tool results are all just nested strings, so a transcript or
    /// a serialized [`crate::orchestration::RunRecord`] is covered by a
    /// single call.
    ///
    /// Idempotent on output: the named `<redacted:<pattern>:<len>>` and
    /// `[redacted]` placeholders do not re-match, so running twice
    /// yields byte-identical JSON (the returned manifest still re-lists
    /// sensitive-named fields, which are re-stamped to the same
    /// placeholder).
    ///
    /// Paths are JSON-path-ish (`$.a.b[0].c`), rooted at `$`.
    pub fn redact_json_manifest(&self, value: &mut JsonValue) -> Vec<RedactionEntry> {
        let mut entries = Vec::new();
        self.redact_json_manifest_at(value, "$", &mut entries);
        entries
    }

    fn redact_json_manifest_at(
        &self,
        value: &mut JsonValue,
        path: &str,
        entries: &mut Vec<RedactionEntry>,
    ) {
        match value {
            JsonValue::Object(map) => {
                let keys = map.keys().cloned().collect::<Vec<_>>();
                for key in keys {
                    let child_path = json_path_child(path, &key);
                    if self.field_is_sensitive(&key) {
                        map.insert(key, JsonValue::String(REDACTED_PLACEHOLDER.to_string()));
                        entries.push(RedactionEntry {
                            path: child_path,
                            class: "sensitive_field".to_string(),
                            action: "replaced".to_string(),
                            replacement: Some(REDACTED_PLACEHOLDER.to_string()),
                        });
                    } else if let Some(child) = map.get_mut(&key) {
                        self.redact_json_manifest_at(child, &child_path, entries);
                    }
                }
            }
            JsonValue::Array(items) => {
                for (index, item) in items.iter_mut().enumerate() {
                    self.redact_json_manifest_at(item, &format!("{path}[{index}]"), entries);
                }
            }
            JsonValue::String(text) => {
                let redacted = self.redact_string(text);
                if let Cow::Owned(replacement) = redacted {
                    // Record the actual replacement string (a named
                    // `<redacted:<pattern>:<len>>` placeholder from the
                    // OA-06 catalog) so audit consumers can attribute
                    // the leak to a specific provider.
                    let manifest_replacement = replacement.clone();
                    *text = replacement;
                    entries.push(RedactionEntry {
                        path: path.to_string(),
                        class: "secret_pattern_or_url".to_string(),
                        action: "replaced".to_string(),
                        replacement: Some(manifest_replacement),
                    });
                }
            }
            _ => {}
        }
    }

    /// Locate the first high-confidence secret that would still be
    /// redacted by this policy anywhere in `value`, without mutating it.
    ///
    /// This is the share/ingest gate: a caller runs
    /// [`RedactionPolicy::redact_json_manifest`] to scrub, then calls
    /// this on the result and refuses to publish if it returns `Some`.
    /// It reuses the exact leaf predicate the redactor uses, so "was
    /// scrubbed" and "would be rejected" can never disagree.
    pub fn find_unredacted_secret(&self, value: &JsonValue) -> Option<UnredactedSecret> {
        self.find_unredacted_secret_at(value, "$")
    }

    fn find_unredacted_secret_at(&self, value: &JsonValue, path: &str) -> Option<UnredactedSecret> {
        match value {
            JsonValue::Object(map) => {
                for (key, child) in map {
                    if let Some(found) =
                        self.find_unredacted_secret_at(child, &json_path_child(path, key))
                    {
                        return Some(found);
                    }
                }
                None
            }
            JsonValue::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    if let Some(found) =
                        self.find_unredacted_secret_at(item, &format!("{path}[{index}]"))
                    {
                        return Some(found);
                    }
                }
                None
            }
            JsonValue::String(text) => {
                if matches!(self.redact_string(text), Cow::Owned(_)) {
                    Some(UnredactedSecret {
                        path: path.to_string(),
                        excerpt: secret_excerpt(text),
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// Bounded, non-sensitive prefix of a value for error messages.
fn secret_excerpt(text: &str) -> String {
    let excerpt = text.chars().take(80).collect::<String>();
    if text.chars().count() > 80 {
        format!("{excerpt}...")
    } else {
        excerpt
    }
}

/// Append `key` to a JSON path, using dotted form for identifier-safe
/// keys and bracketed/quoted form otherwise.
pub(crate) fn json_path_child(parent: &str, key: &str) -> String {
    if key
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        format!("{parent}.{key}")
    } else {
        format!(
            "{parent}[{}]",
            serde_json::to_string(key).unwrap_or_default()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Fake secrets are assembled at runtime so this source file does
    // not itself trip push-protection or secret scanners. Each still
    // matches the redactor's catalog regexes.
    fn aws_key() -> String {
        format!("AKIA{}", "ABCDEFGHIJKLMNOP")
    }
    fn github_pat() -> String {
        format!("ghp_{}", "a".repeat(36))
    }
    fn stripe_key() -> String {
        let head = ["sk", "live"].join("_");
        format!("{head}_{}", "abcdefghijklmnopqrstuvwxyz")
    }
    fn private_key_block() -> String {
        "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----".to_string()
    }

    /// A realistic agent transcript: system + user + assistant with a
    /// tool_use whose input carries a credential, and a tool_result
    /// body that leaks several provider secrets — the highest-risk
    /// carriers the export path must scrub.
    fn dirty_transcript() -> JsonValue {
        json!({
            "_type": "transcript",
            "messages": [
                { "role": "system", "content": "You are a coding agent. Commit is 903e58f1b0a4c2d3e4f5061728394a5b6c7d8e9f." },
                { "role": "user", "content": "deploy with AWS creds" },
                {
                    "role": "assistant",
                    "content": [
                        { "type": "text", "text": "Running the deploy." },
                        {
                            "type": "tool_use",
                            "id": "toolu_01",
                            "name": "run_command",
                            "input": {
                                "command": "aws deploy",
                                "api_key": aws_key(),
                                "env": { "AWS_ACCESS_KEY_ID": aws_key() }
                            }
                        }
                    ]
                },
                {
                    "role": "tool",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "toolu_01",
                            "content": format!(
                                "auth: Bearer abcDEF123_-longenoughtoken\ngithub token {}\nstripe {}\n{}\nrequest_id 550e8400-e29b-41d4-a716-446655440000",
                                github_pat(), stripe_key(), private_key_block()
                            )
                        }
                    ]
                }
            ],
            "summary": "deployed ok"
        })
    }

    fn secrets() -> Vec<String> {
        vec![
            aws_key(),
            github_pat(),
            stripe_key(),
            "b3BlbnNzaC1rZXktdjEAAAAA".to_string(),
        ]
    }

    #[test]
    fn redact_json_manifest_scrubs_every_secret_and_records_paths() {
        crate::reset_thread_local_state();
        let policy = RedactionPolicy::default();
        let mut transcript = dirty_transcript();
        let manifest = policy.redact_json_manifest(&mut transcript);

        let rendered = serde_json::to_string(&transcript).unwrap();
        for secret in secrets() {
            assert!(
                !rendered.contains(&secret),
                "secret leaked into redacted transcript: {secret}\n{rendered}"
            );
        }
        assert!(!manifest.is_empty(), "expected a non-empty manifest");
        // The sensitive-named `api_key` field is replaced wholesale and
        // attributed as a field-name redaction.
        assert!(manifest
            .iter()
            .any(|entry| entry.path.ends_with(".api_key") && entry.class == "sensitive_field"));
        // The tool_result body is a free-form string scrubbed by
        // pattern, attributed with the named replacement.
        assert!(manifest.iter().any(|entry| {
            entry.class == "secret_pattern_or_url"
                && entry
                    .replacement
                    .as_deref()
                    .is_some_and(|value| value.contains("<redacted:"))
        }));
    }

    #[test]
    fn redact_json_manifest_preserves_non_secret_content() {
        crate::reset_thread_local_state();
        let policy = RedactionPolicy::default();
        let mut transcript = dirty_transcript();
        policy.redact_json_manifest(&mut transcript);
        let rendered = serde_json::to_string(&transcript).unwrap();

        // System text and the summary are untouched.
        assert!(rendered.contains("You are a coding agent"));
        assert!(rendered.contains("deployed ok"));
        // False-positive guards: a 40-char git SHA, a UUID request id,
        // and the literal `Running the deploy.` line must survive.
        assert!(rendered.contains("903e58f1b0a4c2d3e4f5061728394a5b6c7d8e9f"));
        assert!(rendered.contains("550e8400-e29b-41d4-a716-446655440000"));
        assert!(rendered.contains("Running the deploy."));
    }

    #[test]
    fn redact_json_manifest_is_idempotent_on_output() {
        crate::reset_thread_local_state();
        let policy = RedactionPolicy::default();
        let mut once = dirty_transcript();
        policy.redact_json_manifest(&mut once);
        let after_first = serde_json::to_string(&once).unwrap();

        let mut twice = once.clone();
        policy.redact_json_manifest(&mut twice);
        let after_second = serde_json::to_string(&twice).unwrap();

        assert_eq!(
            after_first, after_second,
            "second redaction pass must not further mangle already-redacted output"
        );
    }

    #[test]
    fn find_unredacted_secret_flags_raw_then_clears_after_redaction() {
        crate::reset_thread_local_state();
        let policy = RedactionPolicy::default();
        let mut transcript = dirty_transcript();

        let found = policy
            .find_unredacted_secret(&transcript)
            .expect("raw transcript still carries a secret");
        assert!(found.path.starts_with("$."));
        assert!(!found.excerpt.is_empty());

        policy.redact_json_manifest(&mut transcript);
        assert!(
            policy.find_unredacted_secret(&transcript).is_none(),
            "no secret should remain after redaction"
        );
    }

    #[test]
    fn find_unredacted_secret_ignores_benign_ids() {
        crate::reset_thread_local_state();
        let policy = RedactionPolicy::default();
        let benign = json!({
            "git_sha": "903e58f1b0a4c2d3e4f5061728394a5b6c7d8e9f",
            "uuid": "550e8400-e29b-41d4-a716-446655440000",
            "note": "kept 12 messages, added 3, then replied in text",
            "max_tokens": "max_tokens=200",
        });
        assert!(policy.find_unredacted_secret(&benign).is_none());
    }
}
