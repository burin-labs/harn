//! Per-case execution policy for conformance fixtures.

use std::fs;
use std::path::Path;

use serde::Deserialize;

const MAX_DECLARED_TIMEOUT_MS: u64 = 600_000;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ConformanceCostClass {
    ExternalWait,
}

impl ConformanceCostClass {
    fn label(self) -> &'static str {
        match self {
            Self::ExternalWait => "external_wait",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConformanceDeadlineDeclaration {
    cost_class: ConformanceCostClass,
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConformanceCaseConfig {
    deadline: ConformanceDeadlineDeclaration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ResolvedConformanceDeadline {
    pub(super) limit_ms: u64,
    declaration: Option<(ConformanceCostClass, u64)>,
}

impl ResolvedConformanceDeadline {
    fn suite_default(limit_ms: u64) -> Self {
        Self {
            limit_ms,
            declaration: None,
        }
    }

    pub(super) fn hang_message(self, relative_path: &str) -> String {
        match self.declaration {
            Some((cost_class, declared_ms)) => format!(
                "{relative_path}: hang deadline exceeded after {}ms \
                 (cost class {}; declared {declared_ms}ms)",
                self.limit_ms,
                cost_class.label(),
            ),
            None => format!(
                "{relative_path}: hang deadline exceeded after {}ms",
                self.limit_ms
            ),
        }
    }
}

pub(super) fn config_path(harn_file: &Path) -> std::path::PathBuf {
    harn_file.with_extension("case.json")
}

pub(super) fn resolve_deadline(
    harn_file: &Path,
    suite_timeout_ms: u64,
) -> Result<ResolvedConformanceDeadline, String> {
    let path = config_path(harn_file);
    if !path.is_file() {
        return Ok(ResolvedConformanceDeadline::suite_default(suite_timeout_ms));
    }
    let source = fs::read_to_string(&path)
        .map_err(|error| format!("read conformance case config {}: {error}", path.display()))?;
    let config: ConformanceCaseConfig = serde_json::from_str(&source)
        .map_err(|error| format!("parse conformance case config {}: {error}", path.display()))?;
    let declared = config.deadline.timeout_ms;
    if declared == 0 || declared > MAX_DECLARED_TIMEOUT_MS {
        return Err(format!(
            "conformance case config {} deadline.timeout_ms must be in 1..={MAX_DECLARED_TIMEOUT_MS}",
            path.display()
        ));
    }
    Ok(ResolvedConformanceDeadline {
        limit_ms: suite_timeout_ms.max(declared),
        declaration: Some((config.deadline.cost_class, declared)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_deadline_never_weakens_the_suite_backstop() {
        let temp = tempfile::tempdir().unwrap();
        let harn_file = temp.path().join("wait.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();
        fs::write(
            config_path(&harn_file),
            r#"{"deadline":{"cost_class":"external_wait","timeout_ms":120000}}"#,
        )
        .unwrap();

        assert_eq!(
            resolve_deadline(&harn_file, 60_000).unwrap().limit_ms,
            120_000
        );
        assert_eq!(
            resolve_deadline(&harn_file, 180_000).unwrap().limit_ms,
            180_000
        );
    }

    #[test]
    fn malformed_or_unbounded_declarations_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let harn_file = temp.path().join("wait.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();
        for timeout_ms in [0, MAX_DECLARED_TIMEOUT_MS + 1] {
            fs::write(
                config_path(&harn_file),
                format!(
                    r#"{{"deadline":{{"cost_class":"external_wait","timeout_ms":{timeout_ms}}}}}"#
                ),
            )
            .unwrap();
            assert!(resolve_deadline(&harn_file, 60_000).is_err());
        }
    }
}
