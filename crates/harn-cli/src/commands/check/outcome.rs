use harn_lint::LintSeverity;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CommandOutcome {
    pub has_error: bool,
    pub has_warning: bool,
}

impl CommandOutcome {
    pub(crate) fn should_fail(self, strict: bool) -> bool {
        self.has_error || (strict && self.has_warning)
    }
}

pub(super) fn print_lint_diagnostics(
    path: &str,
    source: &str,
    diagnostics: &[harn_lint::LintDiagnostic],
) -> bool {
    let mut has_error = false;
    for diag in diagnostics {
        let severity = match diag.severity {
            LintSeverity::Warning => "warning",
            LintSeverity::Error => {
                has_error = true;
                "error"
            }
        };
        let rendered = harn_parser::diagnostic::render_diagnostic(
            source,
            path,
            &diag.span,
            severity,
            &diag.message,
            Some(&format!("lint[{}]", diag.rule)),
            diag.suggestion.as_deref(),
        );
        eprint!("{rendered}");
    }
    has_error
}
