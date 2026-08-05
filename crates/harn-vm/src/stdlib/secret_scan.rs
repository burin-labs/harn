use std::collections::BTreeMap;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub use harn_kernel::pure::SecretFinding;

pub const SECRET_SCAN_AUDIT_TOPIC: &str = "audit.secret_scan";

pub fn scan_content(content: &str) -> Vec<SecretFinding> {
    harn_kernel::pure::scan_secrets(content)
}

pub async fn append_secret_scan_audit<L: EventLog + ?Sized>(
    event_log: &L,
    caller: &str,
    content_len: usize,
    findings: &[SecretFinding],
) -> Result<(), crate::event_log::LogError> {
    let payload = serde_json::json!({
        "caller": caller,
        "content_len": content_len,
        "finding_count": findings.len(),
        "clean": findings.is_empty(),
        "findings": findings
            .iter()
            .map(|finding| {
                serde_json::json!({
                    "detector": finding.detector,
                    "source": finding.source,
                    "title": finding.title,
                    "precision": finding.precision,
                    "line": finding.line,
                    "column_start": finding.column_start,
                    "column_end": finding.column_end,
                    "start_offset": finding.start_offset,
                    "end_offset": finding.end_offset,
                    "fingerprint": finding.fingerprint,
                    "redacted": finding.redacted,
                })
            })
            .collect::<Vec<_>>(),
        "observed_at": crate::orchestration::now_unix_seconds_text(),
    });
    let topic = Topic::new(SECRET_SCAN_AUDIT_TOPIC).expect("secret scan audit topic is valid");
    let kind = if findings.is_empty() {
        "scan_clean"
    } else {
        "scan_detected"
    };
    event_log
        .append(&topic, LogEvent::new(kind, payload))
        .await?;
    Ok(())
}

pub async fn audit_secret_scan_active(
    caller: &str,
    content_len: usize,
    findings: &[SecretFinding],
) {
    emit_secret_scan_log(caller, content_len, findings);

    let Some(event_log) = active_event_log() else {
        return;
    };

    if let Err(error) =
        append_secret_scan_audit(event_log.as_ref(), caller, content_len, findings).await
    {
        crate::events::log_warn(
            "secret_scan.audit",
            &format!("failed to append secret scan audit event: {error}"),
        );
    }
}

pub(crate) fn register_secret_scan_builtins(vm: &mut Vm) {
    vm.register_builtin_def(&SECRET_SCAN_IMPL_DEF);
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig_expr = harn_builtin_meta::signatures::PORTABLE_SECRET_SCAN,
    category = "security"
)]
fn secret_scan_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let content = match args.first() {
        Some(VmValue::Nil) | None => {
            return Err(VmError::Runtime("secret_scan: content is required".into()));
        }
        Some(value) => value.display(),
    };
    let findings = scan_content(&content);
    let value = serde_json::to_value(findings)
        .map_err(|error| VmError::Runtime(format!("secret_scan: {error}")))?;
    Ok(crate::schema::json_to_vm_value(&value))
}

fn emit_secret_scan_log(caller: &str, content_len: usize, findings: &[SecretFinding]) {
    let metadata = serde_json::json!({
        "topic": SECRET_SCAN_AUDIT_TOPIC,
        "caller": caller,
        "content_len": content_len,
        "finding_count": findings.len(),
        "clean": findings.is_empty(),
        "findings": findings
            .iter()
            .map(|finding| serde_json::json!({
                "detector": finding.detector,
                "source": finding.source,
                "line": finding.line,
                "fingerprint": finding.fingerprint,
                "redacted": finding.redacted,
            }))
            .collect::<Vec<_>>(),
    });
    let metadata = metadata
        .as_object()
        .cloned()
        .map(|object| object.into_iter().collect::<BTreeMap<_, _>>())
        .unwrap_or_default();
    crate::events::log_info_meta("secret_scan.audit", "secret scan completed", metadata);
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::event_log::{EventLog, MemoryEventLog};
    use std::collections::BTreeSet;

    #[test]
    fn scan_content_detects_specific_rules_and_entropy_rule() {
        let findings = scan_content(
            r#"
github_token = "ghp_1234567890abcdefghijklmnopqrstuvwxyzAB"
config = { client_secret: "QWxhZGRpbjpPcGVuU2VzYW1lQWNjZXNzVG9rZW4=" }
"#,
        );

        assert!(findings
            .iter()
            .any(|finding| finding.detector == "github-token"));
        assert!(findings
            .iter()
            .any(|finding| finding.detector == "high-entropy-credential-assignment"));
        assert!(!findings
            .iter()
            .any(|finding| finding.detector == "sensitive-assignment"));
    }

    #[test]
    fn scan_content_deduplicates_generic_assignment_overlaps() {
        let findings = scan_content(r#"token = "ghp_1234567890abcdefghijklmnopqrstuvwxyzAB""#);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].detector, "github-token");
    }

    #[test]
    fn precision_class_splits_token_shapes_from_keyword_heuristics() {
        let findings = scan_content(
            "ghp_1234567890abcdefghijklmnopqrstuvwxyzAB\npassword = \"s3cr3t-value-here\"",
        );
        let precision = |detector: &str| {
            findings
                .iter()
                .find(|finding| finding.detector == detector)
                .map(|finding| finding.precision.as_str())
        };
        // A self-identifying token shape is high precision (safe to hard-block).
        assert_eq!(precision("github-token"), Some("high"));
        // A keyword/context match is heuristic (redaction-only, over-blocks).
        assert_eq!(precision("sensitive-assignment"), Some("heuristic"));
        // Every finding is classified.
        assert!(findings
            .iter()
            .all(|finding| finding.precision == "high" || finding.precision == "heuristic"));
    }

    #[test]
    fn scan_content_keeps_generic_assignment_without_specific_detector() {
        let findings = scan_content(r#"token = "secret123""#);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].detector, "sensitive-assignment");
    }

    #[test]
    fn scan_content_preserves_source_declarations_with_secretish_identifiers() {
        let findings = scan_content("pub const Token = struct { kind: u8 };\n");
        assert!(findings.is_empty());
    }

    #[test]
    fn scan_content_redacts_private_key_blocks() {
        let findings = scan_content(
            "-----BEGIN OPENSSH PRIVATE KEY-----\nZXhhbXBsZQ==\n-----END OPENSSH PRIVATE KEY-----\n",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].detector, "private-key-block");
        assert_eq!(
            findings[0].end_offset - findings[0].start_offset,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nZXhhbXBsZQ==\n-----END OPENSSH PRIVATE KEY-----"
                .len()
        );
        assert_eq!(
            findings[0].redacted,
            "-----BEGIN OPENSSH PRIVATE KEY----- …"
        );
    }

    #[test]
    fn scan_content_covers_redaction_only_token_shapes() {
        let findings = scan_content(
            "Authorization: Bearer abcDEFghi123_-+/=xyz\njwt=eyJabcd.eyJefgh.signature_pad\n",
        );
        let detectors = findings
            .iter()
            .map(|finding| finding.detector.as_str())
            .collect::<BTreeSet<_>>();
        assert!(detectors.contains("bearer-token"));
        assert!(detectors.contains("jwt-token"));
    }

    #[test]
    fn scan_content_covers_ai_provider_token_shapes() {
        let huggingface = format!("hf_{}", "a".repeat(24));
        let cerebras = format!("csk-{}", "b".repeat(48));
        let together = format!("tgp_v1_{}", "c".repeat(32));
        let google = format!("AIza{}", "D".repeat(35));
        let content = format!("{huggingface}\n{cerebras}\n{together}\n{google}\n");

        let findings = scan_content(&content);
        let detectors = findings
            .iter()
            .map(|finding| (finding.detector.as_str(), finding.source.as_str()))
            .collect::<BTreeSet<_>>();

        assert!(detectors.contains(&("huggingface-token", "huggingface-docs")));
        assert!(detectors.contains(&("cerebras-api-key", "cerebras-docs")));
        assert!(detectors.contains(&("together-api-key", "together-bug-report")));
        assert!(detectors.contains(&("google-api-key", "microsoft-purview")));
        for secret in [&huggingface, &cerebras, &together, &google] {
            assert!(!findings
                .iter()
                .any(|finding| finding.redacted.contains(secret)));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_secret_scan_audit_writes_redacted_event() {
        let log = MemoryEventLog::new(32);
        let findings = scan_content(r#"token = "sk-abcdefghijklmnopqrstuvwx123456""#);
        append_secret_scan_audit(&log, "test.secret_scan", 44, &findings)
            .await
            .unwrap();

        let topic = Topic::new(SECRET_SCAN_AUDIT_TOPIC).unwrap();
        let events = log.read_range(&topic, None, 10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1.kind, "scan_detected");
        assert_eq!(events[0].1.payload["caller"], "test.secret_scan");
        let redacted = events[0].1.payload["findings"][0]["redacted"]
            .as_str()
            .unwrap();
        assert!(redacted.contains('…'));
        assert!(!redacted.contains("abcdefghijklmnopqrstuvwx123456"));
    }
}
