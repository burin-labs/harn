use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub const SECRET_SCAN_AUDIT_TOPIC: &str = "audit.secret_scan";
const HIGH_ENTROPY_THRESHOLD: f64 = 3.5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretFinding {
    pub detector: String,
    pub source: String,
    pub title: String,
    pub line: usize,
    pub column_start: usize,
    pub column_end: usize,
    pub start_offset: usize,
    pub end_offset: usize,
    pub redacted: String,
    pub fingerprint: String,
}

struct SecretRule {
    detector: &'static str,
    source: &'static str,
    title: &'static str,
    regex: Regex,
}

static SECRET_RULES: LazyLock<Vec<SecretRule>> = LazyLock::new(|| {
    vec![
        SecretRule {
            detector: "aws-access-key-id",
            source: "gitleaks",
            title: "AWS access key id",
            regex: Regex::new(r"\b(?:AKIA|ASIA|AGPA|AIDA|ANPA|AROA|AIPA)[A-Z0-9]{16}\b").unwrap(),
        },
        SecretRule {
            detector: "github-token",
            source: "gitleaks",
            title: "GitHub token",
            regex: Regex::new(r"\bgh(?:p|o|u|s|r)_[A-Za-z0-9]{36,255}\b").unwrap(),
        },
        SecretRule {
            detector: "github-fine-grained-token",
            source: "gitleaks",
            title: "GitHub fine-grained personal access token",
            regex: Regex::new(r"\bgithub_pat_[A-Za-z0-9_]{20,255}\b").unwrap(),
        },
        SecretRule {
            detector: "gitlab-token",
            source: "detect-secrets",
            title: "GitLab personal access token",
            regex: Regex::new(r"\bglpat-[A-Za-z0-9_-]{20,255}\b").unwrap(),
        },
        SecretRule {
            detector: "npm-token",
            source: "detect-secrets",
            title: "npm access token",
            regex: Regex::new(r"\bnpm_[A-Za-z0-9]{36}\b").unwrap(),
        },
        SecretRule {
            detector: "openai-api-key",
            source: "detect-secrets",
            title: "OpenAI API key",
            regex: Regex::new(r"\bsk-[A-Za-z0-9_-]{20,255}\b").unwrap(),
        },
        SecretRule {
            detector: "slack-token",
            source: "trufflehog",
            title: "Slack token",
            regex: Regex::new(r"\bxox(?:a|b|p|r|s)-[A-Za-z0-9-]{10,255}\b").unwrap(),
        },
        SecretRule {
            detector: "stripe-secret-key",
            source: "trufflehog",
            title: "Stripe secret or restricted key",
            regex: Regex::new(r"\b(?:rk|sk)_(?:live|test)_[0-9A-Za-z]{16,255}\b").unwrap(),
        },
        SecretRule {
            detector: "private-key-block",
            source: "detect-secrets",
            title: "Private key block",
            regex: Regex::new(r"(?m)^-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----$").unwrap(),
        },
    ]
});

static HIGH_ENTROPY_ASSIGNMENT_RULE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?im)(?:secret|token|api[_-]?key|access[_-]?key|password|passwd|pwd|client[_-]?secret|private[_-]?key)[^\n:=]{0,32}(?::|=)\s*["']([A-Za-z0-9+/=_\.-]{20,})["']"#,
    )
    .unwrap()
});

pub fn scan_content(content: &str) -> Vec<SecretFinding> {
    let line_starts = line_starts(content);
    let mut findings = Vec::new();

    for rule in SECRET_RULES.iter() {
        for mat in rule.regex.find_iter(content) {
            findings.push(build_finding(
                content,
                &line_starts,
                rule.detector,
                rule.source,
                rule.title,
                mat.start(),
                mat.end(),
                mat.as_str(),
            ));
        }
    }

    for captures in HIGH_ENTROPY_ASSIGNMENT_RULE.captures_iter(content) {
        let Some(secret) = captures.get(1) else {
            continue;
        };
        if shannon_entropy(secret.as_str()) < HIGH_ENTROPY_THRESHOLD {
            continue;
        }
        findings.push(build_finding(
            content,
            &line_starts,
            "high-entropy-credential-assignment",
            "trufflehog",
            "High-entropy secret assignment",
            secret.start(),
            secret.end(),
            secret.as_str(),
        ));
    }

    findings.sort_by(|left, right| {
        left.start_offset
            .cmp(&right.start_offset)
            .then(left.end_offset.cmp(&right.end_offset))
            .then(left.detector.cmp(&right.detector))
    });
    let specific_spans: BTreeSet<(usize, usize)> = findings
        .iter()
        .filter(|finding| finding.detector != "high-entropy-credential-assignment")
        .map(|finding| (finding.start_offset, finding.end_offset))
        .collect();
    findings.retain(|finding| {
        finding.detector != "high-entropy-credential-assignment"
            || !specific_spans.contains(&(finding.start_offset, finding.end_offset))
    });
    findings.dedup_by(|left, right| {
        left.detector == right.detector
            && left.start_offset == right.start_offset
            && left.end_offset == right.end_offset
    });
    findings
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
        "observed_at": crate::orchestration::now_rfc3339(),
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
    vm.register_async_builtin("secret_scan", |args| async move {
        let content = match args.first() {
            Some(VmValue::Nil) | None => {
                return Err(VmError::Runtime("secret_scan: content is required".into()));
            }
            Some(value) => value.display(),
        };

        let findings = scan_content(&content);
        audit_secret_scan_active("stdlib.secret_scan", content.len(), &findings).await;

        let value = serde_json::to_value(findings)
            .map_err(|error| VmError::Runtime(format!("secret_scan: {error}")))?;
        Ok(crate::schema::json_to_vm_value(&value))
    });
}

fn build_finding(
    content: &str,
    line_starts: &[usize],
    detector: &str,
    source: &str,
    title: &str,
    start_offset: usize,
    end_offset: usize,
    matched: &str,
) -> SecretFinding {
    let (line, column_start) = offset_to_line_col(content, line_starts, start_offset);
    let (_, column_end) = offset_to_line_col(content, line_starts, end_offset);
    SecretFinding {
        detector: detector.to_string(),
        source: source.to_string(),
        title: title.to_string(),
        line,
        column_start,
        column_end,
        start_offset,
        end_offset,
        redacted: redact_match(matched),
        fingerprint: fingerprint(matched),
    }
}

fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn offset_to_line_col(content: &str, line_starts: &[usize], offset: usize) -> (usize, usize) {
    let line_index = line_starts
        .partition_point(|start| *start <= offset)
        .saturating_sub(1);
    let line_start = line_starts[line_index];
    let column = content[line_start..offset].chars().count() + 1;
    (line_index + 1, column)
}

fn redact_match(matched: &str) -> String {
    if matched.starts_with("-----BEGIN ") {
        return format!(
            "{} …",
            matched
                .lines()
                .next()
                .unwrap_or("-----BEGIN PRIVATE KEY-----")
        );
    }

    let chars: Vec<char> = matched.chars().collect();
    if chars.len() <= 8 {
        return "*".repeat(chars.len());
    }
    let prefix: String = chars.iter().take(4).collect();
    let suffix: String = chars[chars.len().saturating_sub(4)..].iter().collect();
    format!("{prefix}…{suffix}")
}

fn fingerprint(matched: &str) -> String {
    let hash = sha2::Sha256::digest(matched.as_bytes());
    let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    hex[..16].to_string()
}

fn shannon_entropy(value: &str) -> f64 {
    let mut counts = BTreeMap::new();
    for ch in value.chars() {
        *counts.entry(ch).or_insert(0usize) += 1;
    }
    let len = value.chars().count() as f64;
    counts
        .values()
        .map(|count| {
            let probability = *count as f64 / len;
            -(probability * probability.log2())
        })
        .sum()
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
    }

    #[test]
    fn scan_content_redacts_private_key_blocks() {
        let findings = scan_content(
            "-----BEGIN OPENSSH PRIVATE KEY-----\nZXhhbXBsZQ==\n-----END OPENSSH PRIVATE KEY-----\n",
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].detector, "private-key-block");
        assert_eq!(
            findings[0].redacted,
            "-----BEGIN OPENSSH PRIVATE KEY----- …"
        );
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
