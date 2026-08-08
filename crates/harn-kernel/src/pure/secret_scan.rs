use std::collections::BTreeMap;
use std::sync::OnceLock;

use harn_secret_catalog::{SecretPatternSpec, DEFAULT_SECRET_PATTERN_SPECS, PRECISION_HEURISTIC};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::Digest;

const HIGH_ENTROPY_THRESHOLD: f64 = 3.5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretFinding {
    pub detector: String,
    pub source: String,
    pub title: String,
    pub precision: String,
    pub line: usize,
    pub column_start: usize,
    pub column_end: usize,
    pub start_offset: usize,
    pub end_offset: usize,
    pub redacted: String,
    pub fingerprint: String,
}

/// One catalog entry paired with its process-wide compiled matcher.
///
/// Scanning and redaction are different questions asked of the same catalog, so
/// they share one compiled copy rather than each paying to build their own.
pub struct CompiledSecretPattern {
    pub spec: &'static SecretPatternSpec,
    pub regex: Regex,
}

static DEFAULT_PATTERNS: OnceLock<Vec<CompiledSecretPattern>> = OnceLock::new();
static HIGH_ENTROPY_ASSIGNMENT: OnceLock<Regex> = OnceLock::new();

/// The shared catalog, compiled on first use.
pub fn compiled_secret_patterns() -> &'static [CompiledSecretPattern] {
    DEFAULT_PATTERNS.get_or_init(|| {
        DEFAULT_SECRET_PATTERN_SPECS
            .iter()
            .map(|spec| CompiledSecretPattern {
                spec,
                regex: Regex::new(spec.regex).unwrap_or_else(|error| {
                    panic!("invalid {} secret regex: {error}", spec.detector)
                }),
            })
            .collect()
    })
}

/// Whether the catalog has already been compiled. Hosts that warm it eagerly
/// during startup assert against this so the cost cannot silently move back
/// onto a deep call stack.
pub fn secret_patterns_compiled() -> bool {
    DEFAULT_PATTERNS.get().is_some()
}

fn high_entropy_assignment() -> &'static Regex {
    HIGH_ENTROPY_ASSIGNMENT.get_or_init(|| {
        Regex::new(
            r#"(?im)(?:secret|token|api[_-]?key|access[_-]?key|password|passwd|pwd|client[_-]?secret|private[_-]?key)[^\n:=]{0,32}(?::|=)\s*["']([A-Za-z0-9+/=_\.-]{20,})["']"#,
        )
        .expect("high-entropy secret pattern is valid")
    })
}

pub fn scan_secrets(content: &str) -> Vec<SecretFinding> {
    let line_starts = line_starts(content);
    let mut findings = Vec::new();

    for rule in compiled_secret_patterns() {
        for matched in rule.regex.find_iter(content) {
            findings.push(build_finding(
                content,
                &line_starts,
                rule.spec.detector,
                rule.spec.source,
                rule.spec.title,
                rule.spec.precision,
                matched.start(),
                matched.end(),
                matched.as_str(),
            ));
        }
    }

    for captures in high_entropy_assignment().captures_iter(content) {
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
            PRECISION_HEURISTIC,
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
    let spans = findings
        .iter()
        .map(|finding| {
            (
                finding.start_offset,
                finding.end_offset,
                detector_specificity(&finding.detector),
            )
        })
        .collect::<Vec<_>>();
    findings.retain(|finding| {
        let specificity = detector_specificity(&finding.detector);
        !spans.iter().any(|(start, end, other_specificity)| {
            *other_specificity > specificity
                && finding.start_offset < *end
                && *start < finding.end_offset
        })
    });
    findings.dedup_by(|left, right| {
        left.detector == right.detector
            && left.start_offset == right.start_offset
            && left.end_offset == right.end_offset
    });
    findings
}

fn detector_specificity(detector: &str) -> u8 {
    match detector {
        "sensitive-assignment" => 0,
        "high-entropy-credential-assignment" => 1,
        _ => 2,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_finding(
    content: &str,
    line_starts: &[usize],
    detector: &str,
    source: &str,
    title: &str,
    precision: &str,
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
        precision: precision.to_string(),
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
    std::iter::once(0)
        .chain(
            content
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        )
        .collect()
}

#[expect(
    clippy::string_slice,
    reason = "line starts and regex match offsets are char boundaries"
)]
fn offset_to_line_col(content: &str, starts: &[usize], offset: usize) -> (usize, usize) {
    let line_index = starts
        .partition_point(|start| *start <= offset)
        .saturating_sub(1);
    let line_start = starts[line_index];
    (
        line_index + 1,
        content[line_start..offset].chars().count() + 1,
    )
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
    let chars = matched.chars().collect::<Vec<_>>();
    if chars.len() <= 8 {
        return "*".repeat(chars.len());
    }
    let prefix = chars.iter().take(4).collect::<String>();
    let suffix = chars[chars.len() - 4..].iter().collect::<String>();
    format!("{prefix}…{suffix}")
}

fn fingerprint(matched: &str) -> String {
    let digest = sha2::Sha256::digest(matched.as_bytes());
    hex::encode(&digest[..8])
}

fn shannon_entropy(value: &str) -> f64 {
    let mut counts = BTreeMap::new();
    for character in value.chars() {
        *counts.entry(character).or_insert(0_usize) += 1;
    }
    let length = value.chars().count() as f64;
    counts
        .values()
        .map(|count| {
            let probability = *count as f64 / length;
            -(probability * probability.log2())
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_and_deduplicates_the_canonical_catalog() {
        let findings = scan_secrets(r#"token = "ghp_1234567890abcdefghijklmnopqrstuvwxyzAB""#);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].detector, "github-token");
        assert_eq!(findings[0].precision, "high");
    }

    #[test]
    fn source_with_secretish_identifiers_remains_clean() {
        assert!(scan_secrets("pub const Token = struct { kind: u8 };\n").is_empty());
    }
}
