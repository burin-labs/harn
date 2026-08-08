pub(super) fn format_conformance_lint_diagnostics(
    diagnostics: &[harn_lint::LintDiagnostic],
) -> String {
    diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "{} {} lint[{}]: {}",
                diagnostic.code,
                severity_label(diagnostic.severity),
                diagnostic.rule,
                diagnostic.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn lint_expectation_error(actual: &str, expected_spec: &str) -> Option<String> {
    let expectations = expected_spec
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if expectations.is_empty() {
        return Some("lint expectation file is empty".to_string());
    }

    let mut failures = Vec::new();
    for expectation in expectations {
        if let Some(forbidden) = expectation.strip_prefix('!') {
            let forbidden = forbidden.trim();
            if forbidden.is_empty() {
                failures.push("negative lint expectation is missing a pattern".to_string());
            } else if actual.contains(forbidden) {
                failures.push(format!("forbidden lint matched: {forbidden}"));
            }
        } else if !actual.contains(expectation) {
            failures.push(format!("required lint missing: {expectation}"));
        }
    }

    (!failures.is_empty()).then(|| failures.join("\n"))
}

fn severity_label(severity: harn_lint::LintSeverity) -> &'static str {
    match severity {
        harn_lint::LintSeverity::Info => "info",
        harn_lint::LintSeverity::Warning => "warning",
        harn_lint::LintSeverity::Error => "error",
    }
}
