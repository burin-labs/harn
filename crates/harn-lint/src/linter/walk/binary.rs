//! Lint rules for binary operator expressions: constant logical operands,
//! self-comparisons, comparisons against boolean literals, and arithmetic on
//! `bool`/`nil` literals.

use harn_lexer::FixEdit;
use harn_parser::{BinaryOperator, DiagnosticCode as Code, Node, SNode};

use super::super::Linter;
use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::fixes::{is_nan_free, is_pure_expression};
use crate::naming::simplify_bool_comparison;

impl<'a> Linter<'a> {
    pub(super) fn lint_binary_op(
        &mut self,
        snode: &SNode,
        op: BinaryOperator,
        left: &SNode,
        right: &SNode,
    ) {
        if let Some((message, replacement)) = Self::constant_logical_reduction(op, left, right) {
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintConstantLogicalOperand,
                rule: "constant-logical-operand".into(),
                message,
                span: snode.span,
                severity: LintSeverity::Warning,
                suggestion: Some(format!("replace this expression with `{replacement}`")),
                fix: Some(vec![FixEdit {
                    span: snode.span,
                    replacement: replacement.to_string(),
                }]),
            });
        }
        let is_self_comparison = matches!(op, BinaryOperator::Eq | BinaryOperator::Neq)
            && left.node == right.node
            && is_pure_expression(&left.node);
        if is_self_comparison {
            let replacement = if matches!(op, BinaryOperator::Eq) {
                "true"
            } else {
                "false"
            };
            // A float operand may be NaN, for which `x == x` is `false`
            // and `x != x` is `true` (the idiomatic NaN test). Folding
            // the comparison to a constant is only sound when the
            // operand provably cannot be a NaN float, so gate both the
            // autofix and the "replace with <const>" suggestion on
            // `is_nan_free`. Otherwise still flag the smell, but leave
            // the code untouched and point at `is_nan(...)`.
            let (suggestion, fix) = if is_nan_free(&left.node) {
                (
                    format!("replace this comparison with `{replacement}`"),
                    Some(vec![FixEdit {
                        span: snode.span,
                        replacement: replacement.to_string(),
                    }]),
                )
            } else {
                (
                    format!(
                        "use `is_nan(...)` to test for NaN; this is otherwise \
                         always `{replacement}` for non-NaN values"
                    ),
                    None,
                )
            };
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintPointlessComparison,
                rule: "pointless-comparison".into(),
                message: format!("expression is compared to itself with `{op}`"),
                span: snode.span,
                severity: LintSeverity::Warning,
                suggestion: Some(suggestion),
                fix,
            });
        }
        if matches!(op, BinaryOperator::Eq | BinaryOperator::Neq) && !is_self_comparison {
            let is_bool_left = matches!(left.node, Node::BoolLiteral(_));
            let is_bool_right = matches!(right.node, Node::BoolLiteral(_));
            let compared_expression = if is_bool_left { right } else { left };
            // Equality with a Boolean literal is redundant only when
            // the other expression is proven to be exactly `bool`.
            // Optional and gradual values have distinct nil behavior,
            // so syntax alone is not enough to classify this repair.
            if (is_bool_left || is_bool_right)
                && self.expression_is_non_optional_bool(compared_expression)
            {
                let (suggestion, msg) = if matches!(op, BinaryOperator::Eq) {
                    if matches!(right.node, Node::BoolLiteral(true))
                        || matches!(left.node, Node::BoolLiteral(true))
                    {
                        (
                            "remove the comparison, use the expression directly",
                            "comparison to `true` is redundant",
                        )
                    } else {
                        ("use `!expr` instead", "comparison to `false` is redundant")
                    }
                } else if matches!(right.node, Node::BoolLiteral(true))
                    || matches!(left.node, Node::BoolLiteral(true))
                {
                    ("use `!expr` instead", "`!= true` is redundant")
                } else {
                    (
                        "remove the comparison, use the expression directly",
                        "`!= false` is redundant",
                    )
                };
                let fix = self.source.and_then(|src| {
                    let expr_text = src.get(snode.span.start..snode.span.end)?;
                    let replacement = simplify_bool_comparison(expr_text)?;
                    Some(vec![FixEdit {
                        span: snode.span,
                        replacement,
                    }])
                });
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintComparisonToBool,
                    rule: "comparison-to-bool".into(),
                    message: msg.to_string(),
                    span: snode.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(suggestion.to_string()),
                    fix,
                });
            }
        }
        if op.is_compound_assignable() {
            let has_bad_literal = matches!(left.node, Node::BoolLiteral(_) | Node::NilLiteral)
                || matches!(right.node, Node::BoolLiteral(_) | Node::NilLiteral);
            if has_bad_literal {
                let fix = if matches!(op, BinaryOperator::Add) {
                    self.source.and_then(|src| {
                        let is_left_str = matches!(&left.node, Node::StringLiteral(_));
                        let is_right_str = matches!(&right.node, Node::StringLiteral(_));
                        if !is_left_str && !is_right_str {
                            return None;
                        }
                        let (str_node, other_node) = if is_left_str {
                            (left, right)
                        } else {
                            (right, left)
                        };
                        let str_text = src.get(str_node.span.start..str_node.span.end)?;
                        let other_text = src.get(other_node.span.start..other_node.span.end)?;
                        let inner = str_text.strip_prefix('"')?.strip_suffix('"')?;
                        let replacement = if is_left_str {
                            format!("\"{inner}${{{other_text}}}\"")
                        } else {
                            format!("\"${{{other_text}}}{inner}\"")
                        };
                        Some(vec![FixEdit {
                            span: snode.span,
                            replacement,
                        }])
                    })
                } else {
                    None
                };
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintInvalidBinaryOpLiteral,
                    rule: "invalid-binary-op-literal".into(),
                    message: format!(
                        "operator '{op}' used with boolean or nil literal — this will cause a runtime error"
                    ),
                    span: snode.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(
                        "use to_string() or string interpolation to convert values explicitly".to_string(),
                    ),
                    fix,
                });
            }
        }
        self.lint_node(left);
        self.lint_node(right);
    }
}
