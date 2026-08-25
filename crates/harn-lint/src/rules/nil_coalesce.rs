//! No-op nil-coalescing rules.
//!
//! The nil-coalescing operator already returns nil when the left side is nil,
//! so a nil fallback is mechanically redundant. Likewise, `x ?? x` is an
//! identity fallback for a pure identifier and communicates uncertainty instead
//! of a real recovery path. A `false` fallback is also redundant as the exact
//! positive condition of `assert`: both `nil` and `false` fail the assertion,
//! while present values retain their native truthiness. These are error-level
//! lints with local fixes that remove the operator and fallback.

use harn_lexer::{FixEdit, Span};
use harn_parser::{visit, BindingPattern, DiagnosticCode as Code, Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const NIL_RULE_NAME: &str = "nil-coalesce-noop";
const SELF_RULE_NAME: &str = "nil-coalesce-self-fallback";

pub(crate) fn check_nil_coalesce_noop(
    _source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let native_assert_is_visible = !shadows_native_assert(program);
    visit::walk_program(program, &mut |node| {
        if let Node::FunctionCall { name, args, .. } = &node.node {
            if native_assert_is_visible && name == "assert" {
                if let Some((left, right)) = args.first().and_then(assert_false_fallback) {
                    diagnostics.push(make_assert_false_diagnostic(left, right));
                }
            }
            return;
        }
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

/// Whether ordinary Harn resolution can bind bare `assert` to source code
/// instead of the native builtin. Unresolved wildcard imports stay
/// conservative: without their export graph, a behavior-preserving fixer must
/// not assume which callable wins.
fn shadows_native_assert(program: &[SNode]) -> bool {
    let mut shadowed = false;
    visit::walk_program(program, &mut |node| {
        shadowed |= match &node.node {
            Node::FnDecl { name, params, .. }
            | Node::Pipeline { name, params, .. }
            | Node::ToolDecl { name, params, .. } => {
                name == "assert" || params.iter().any(|param| param.name == "assert")
            }
            Node::Closure { params, .. } => params.iter().any(|param| param.name == "assert"),
            Node::LetBinding { pattern, .. }
            | Node::ConstBinding { pattern, .. }
            | Node::ForIn { pattern, .. } => binding_pattern_binds_assert(pattern),
            Node::OverrideDecl { name, params, .. } => {
                name == "assert" || params.iter().any(|param| param == "assert")
            }
            Node::TryCatch { error_var, .. } => error_var.as_deref() == Some("assert"),
            Node::SkillDecl { name, .. }
            | Node::StructDecl { name, .. }
            | Node::EnumDecl { name, .. } => name == "assert",
            Node::EvalPackDecl { binding_name, .. } => binding_name == "assert",
            Node::SelectiveImport { names, .. } => names.iter().any(|name| name == "assert"),
            Node::NamespaceImport { alias, .. } => alias == "assert",
            Node::ImportDecl { .. } => true,
            // Match-arm destructuring uses expression-shaped identifiers as
            // bindings. Treat any bare reference conservatively as well: a
            // syntax-only fixer cannot prove that it resolves to the builtin.
            Node::Identifier(name) => name == "assert",
            _ => false,
        };
    });
    shadowed
}

fn binding_pattern_binds_assert(pattern: &BindingPattern) -> bool {
    match pattern {
        BindingPattern::Identifier(name) => name == "assert",
        BindingPattern::Dict(fields) => fields
            .iter()
            .any(|field| field.alias.as_deref().unwrap_or(&field.key) == "assert"),
        BindingPattern::List(elements) => elements.iter().any(|element| element.name == "assert"),
        BindingPattern::Pair(left, right) => left == "assert" || right == "assert",
    }
}

fn assert_false_fallback(argument: &SNode) -> Option<(&SNode, &SNode)> {
    let Node::BinaryOp { op, left, right } = &argument.node else {
        return None;
    };
    (op == "??" && matches!(right.node, Node::BoolLiteral(false)))
        .then_some((left.as_ref(), right.as_ref()))
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

fn make_assert_false_diagnostic(left: &SNode, right: &SNode) -> LintDiagnostic {
    let fix_span = coalesce_fallback_span(left, right);
    LintDiagnostic {
        code: Code::LintNilCoalesceNoop,
        rule: NIL_RULE_NAME.into(),
        message: "`?? false` is redundant in a positive assertion; `assert` already rejects every falsy value"
            .to_string(),
        span: fix_span,
        severity: LintSeverity::Error,
        suggestion: Some("let `assert` apply native truthiness directly".to_string()),
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
