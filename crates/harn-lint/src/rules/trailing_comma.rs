//! `trailing-comma` rule: multiline comma-separated lists (argument
//! lists, list literals, dict/struct literals) must end with a trailing
//! comma on the last item, and single-line lists must not. The actual
//! token-walking detection lives in `harn-fmt` so the formatter and
//! the linter stay in lockstep — `harn fmt` applies the same edits as
//! a post-pass, and `harn lint --fix` surfaces the same diagnostics.

use harn_fmt::{trailing_comma_issues, TrailingCommaKind};
use harn_parser::DiagnosticCode as Code;

use crate::diagnostic::{LintDiagnostic, LintSeverity};

pub(crate) fn check_trailing_comma(source: &str, diagnostics: &mut Vec<LintDiagnostic>) {
    for issue in trailing_comma_issues(source) {
        let (message, suggestion) = match issue.kind {
            TrailingCommaKind::Missing => (
                "multiline comma-separated list is missing a trailing comma",
                "add a trailing comma after the last item",
            ),
            TrailingCommaKind::Extraneous => (
                "single-line comma-separated list has a trailing comma",
                "remove the trailing comma",
            ),
        };
        diagnostics.push(LintDiagnostic {
            code: Code::LintTrailingComma,
            rule: "trailing-comma".into(),
            message: message.to_string(),
            span: issue.edit.span,
            severity: LintSeverity::Warning,
            suggestion: Some(suggestion.to_string()),
            fix: Some(vec![issue.edit]),
        });
    }
}
