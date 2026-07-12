//! `nil-coalesce-noop`: `expr ?? nil` is equivalent to `expr`.
//!
//! The nil-coalescing operator already returns nil when the left side is nil,
//! so a nil fallback is mechanically redundant and usually signals defensive
//! copy-paste. This is a warning-level lint with a local fix that removes the
//! operator and fallback.

use harn_lexer::{FixEdit, Span};
use harn_parser::{visit, DiagnosticCode as Code, Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const RULE_NAME: &str = "nil-coalesce-noop";

pub(crate) fn check_nil_coalesce_noop(
    _source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    visit::walk_program(program, &mut |node| {
        let Node::BinaryOp { op, left, right } = &node.node else {
            return;
        };
        if op != "??" || !matches!(right.node, Node::NilLiteral) {
            return;
        }
        diagnostics.push(make_diagnostic(left, right));
    });
}

fn make_diagnostic(left: &SNode, right: &SNode) -> LintDiagnostic {
    let fix_span = Span {
        start: left.span.end,
        end: right.span.end,
        line: left.span.end_line,
        column: left
            .span
            .column
            .saturating_add(left.span.end.saturating_sub(left.span.start)),
        end_line: right.span.end_line,
    };
    LintDiagnostic {
        code: Code::LintNilCoalesceNoop,
        rule: RULE_NAME.into(),
        message: "`?? nil` is a no-op; the left expression already evaluates to nil when absent"
            .to_string(),
        span: fix_span,
        severity: LintSeverity::Warning,
        suggestion: Some("drop the `?? nil` fallback".to_string()),
        fix: Some(vec![FixEdit {
            span: fix_span,
            replacement: String::new(),
        }]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    fn lint(source: &str) -> Vec<LintDiagnostic> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        let mut diags = Vec::new();
        check_nil_coalesce_noop(source, &program, &mut diags);
        diags
    }

    #[test]
    fn warns_on_nil_fallback() {
        let diags = lint(
            r"
pipeline default(task) {
    const value = task?.flag ?? nil
    log(value)
}
",
        );
        assert_eq!(diags.len(), 1, "diags: {diags:?}");
        assert_eq!(diags[0].rule, RULE_NAME);
        assert_eq!(diags[0].severity, LintSeverity::Warning);
        assert_eq!(diags[0].code, Code::LintNilCoalesceNoop);
    }

    #[test]
    fn ignores_non_nil_fallback() {
        let diags = lint(
            r#"
pipeline default(task) {
    const value = task?.flag ?? "off"
    log(value)
}
"#,
        );
        assert!(diags.is_empty(), "diags: {diags:?}");
    }
}
