use harn_lint::LintSeverity;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CommandOutcome {
    pub has_error: bool,
    pub has_warning: bool,
    /// Total findings emitted for this file.
    pub findings: usize,
    /// Findings carrying a machine-applicable autofix (`fix.is_some()`).
    pub fixable: usize,
}

impl CommandOutcome {
    pub(crate) fn should_fail(self, strict: bool) -> bool {
        self.has_error || (strict && self.has_warning)
    }
}

/// Render each diagnostic to stderr. Returns `(has_error, fixable_count)`,
/// where `fixable_count` mirrors the `diag.fix.is_some()` tally that the JSON
/// report uses so the CLI and JSON surfaces agree on "fixable".
pub(super) fn print_lint_diagnostics(
    path: &str,
    source: &str,
    diagnostics: &[harn_lint::LintDiagnostic],
) -> (bool, usize) {
    let mut has_error = false;
    let mut fixable = 0usize;
    for diag in diagnostics {
        if diag.fix.is_some() {
            fixable += 1;
        }
        let severity = match diag.severity {
            LintSeverity::Info => "info",
            LintSeverity::Warning => "warning",
            LintSeverity::Error => {
                has_error = true;
                "error"
            }
        };
        let rendered = harn_parser::diagnostic::render_diagnostic_with_code(
            source,
            path,
            &diag.span,
            severity,
            diag.code,
            &diag.message,
            Some(&format!("lint[{}]", diag.rule)),
            diag.suggestion.as_deref(),
        );
        eprint!("{rendered}");
    }
    (has_error, fixable)
}
