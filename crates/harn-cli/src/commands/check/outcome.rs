//! Lint/format command outcomes and the shared diagnostic renderer.
//!
//! See [`print_lint_diagnostics`] for the output-channel contract these
//! surfaces share.
#![deny(clippy::print_stdout)]

use harn_lint::LintSeverity;

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CommandOutcome {
    pub has_error: bool,
    pub has_warning: bool,
    /// Total findings emitted for this file.
    pub findings: usize,
    /// Findings carrying a machine-applicable autofix.
    pub fixable: usize,
}

impl CommandOutcome {
    pub(crate) fn should_fail(self, strict: bool) -> bool {
        self.has_error || (strict && self.has_warning)
    }
}

/// Render each diagnostic to stderr. Returns `(has_error, fixable_count)`,
/// where `fixable_count` mirrors the machine-applicable tally that the JSON
/// report uses so the CLI and JSON surfaces agree on "fixable".
///
/// # The output channel
///
/// Every human-readable line `harn lint`, `harn fmt`, and `harn check` produce
/// goes to **stderr**, including the clean-file and applied-fix lines. stdout
/// carries machine-readable output only — the `--json` envelope — so a caller
/// can redirect either stream without silently losing the other.
///
/// Splitting the channel is a silent failure, not a cosmetic one. While the
/// clean-file line was on stdout, `harn lint DIR 2>/dev/null` over a 28-file
/// corpus printed a run of `no issues found` and exited 0 — warnings do not
/// fail without `--strict` — while 114 findings went to the suppressed stream
/// (harn#6168). A genuinely clean corpus produces byte-identical output, so
/// nothing prompts you to doubt it. Sending everything to one stream makes a
/// suppressed sweep silent instead, which reads as what it is.
///
/// Enforced rather than described: the modules that render this output carry
/// `#![deny(clippy::print_stdout)]`, and the ones that also emit a JSON
/// envelope carry it per-function.
pub(super) fn print_lint_diagnostics(
    path: &str,
    source: &str,
    diagnostics: &[harn_lint::LintDiagnostic],
) -> (bool, usize) {
    let (has_error, fixable, rendered) = render_lint_diagnostics(path, source, diagnostics);
    eprint!("{rendered}");
    (has_error, fixable)
}

/// Buffered form of [`print_lint_diagnostics`] for callers that replay text
/// output later (the parallel check driver). Returns
/// `(has_error, fixable_count, rendered_text)`.
pub(super) fn render_lint_diagnostics(
    path: &str,
    source: &str,
    diagnostics: &[harn_lint::LintDiagnostic],
) -> (bool, usize, String) {
    let mut has_error = false;
    let mut fixable = 0usize;
    let mut out = String::new();
    for diag in diagnostics {
        if diag.machine_applicable_fix().is_some() {
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
        out.push_str(&rendered);
    }
    (has_error, fixable, out)
}
