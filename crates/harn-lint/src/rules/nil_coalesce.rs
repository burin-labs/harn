//! No-op nil-coalescing rules.
//!
//! The nil-coalescing operator already returns nil when the left side is nil,
//! so a nil fallback is mechanically redundant. Likewise, `x ?? x` is an
//! identity fallback for a pure identifier and communicates uncertainty instead
//! of a real recovery path. These are error-level lints with local fixes that
//! remove the operator and fallback.

use harn_lexer::{FixEdit, Span};
use harn_parser::{visit, DiagnosticCode as Code, Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const NIL_RULE_NAME: &str = "nil-coalesce-noop";
const SELF_RULE_NAME: &str = "nil-coalesce-self-fallback";

pub(crate) fn check_nil_coalesce_noop(
    _source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    visit::walk_program(program, &mut |node| {
        let Node::BinaryOp { op, left, right } = &node.node else {
            return;
        };
        if op != "??" {
            return;
        }
        if matches!(right.node, Node::NilLiteral) {
            diagnostics.push(make_nil_diagnostic(left, right));
            return;
        }
        if repeated_identifier(left, right) {
            diagnostics.push(make_self_diagnostic(left, right));
        }
    });
}

fn repeated_identifier(left: &SNode, right: &SNode) -> bool {
    matches!(
        (&left.node, &right.node),
        (Node::Identifier(left), Node::Identifier(right)) if left == right
    )
}

fn coalesce_fallback_span(left: &SNode, right: &SNode) -> Span {
    Span {
        start: left.span.end,
        end: right.span.end,
        line: left.span.end_line,
        column: left
            .span
            .column
            .saturating_add(left.span.end.saturating_sub(left.span.start)),
        end_line: right.span.end_line,
    }
}

fn make_nil_diagnostic(left: &SNode, right: &SNode) -> LintDiagnostic {
    let fix_span = coalesce_fallback_span(left, right);
    LintDiagnostic {
        code: Code::LintNilCoalesceNoop,
        rule: NIL_RULE_NAME.into(),
        message: "`?? nil` is a no-op; the left expression already evaluates to nil when absent"
            .to_string(),
        span: fix_span,
        severity: LintSeverity::Error,
        suggestion: Some("drop the `?? nil` fallback".to_string()),
        fix: Some(vec![FixEdit {
            span: fix_span,
            replacement: String::new(),
        }]),
    }
}

fn make_self_diagnostic(left: &SNode, right: &SNode) -> LintDiagnostic {
    let fix_span = coalesce_fallback_span(left, right);
    LintDiagnostic {
        code: Code::LintNilCoalesceSelfFallback,
        rule: SELF_RULE_NAME.into(),
        message: "`x ?? x` is a no-op; the fallback is identical to the left identifier"
            .to_string(),
        span: fix_span,
        severity: LintSeverity::Error,
        suggestion: Some("drop the identity fallback".to_string()),
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
    fn errors_on_nil_fallback() {
        let diags = lint(
            r"
pipeline default(task) {
    const value = task?.flag ?? nil
    log(value)
}
",
        );
        assert_eq!(diags.len(), 1, "diags: {diags:?}");
        assert_eq!(diags[0].rule, NIL_RULE_NAME);
        assert_eq!(diags[0].severity, LintSeverity::Error);
        assert_eq!(diags[0].code, Code::LintNilCoalesceNoop);
    }

    #[test]
    fn errors_on_self_fallback() {
        let diags = lint(
            r"
pipeline default(task) {
    const value = task ?? task
    log(value)
}
",
        );
        assert_eq!(diags.len(), 1, "diags: {diags:?}");
        assert_eq!(diags[0].rule, SELF_RULE_NAME);
        assert_eq!(diags[0].severity, LintSeverity::Error);
        assert_eq!(diags[0].code, Code::LintNilCoalesceSelfFallback);
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

    #[test]
    fn ignores_repeated_calls() {
        let diags = lint(
            r"
pipeline default(task) {
    const value = load() ?? load()
    log(value)
}
",
        );
        assert!(diags.is_empty(), "calls may have effects: {diags:?}");
    }
}
