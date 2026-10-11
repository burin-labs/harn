use crate::ast::{spanned, Node, SNode};
use harn_lexer::Span;

// Precedence levels forward the same allocation. Materializing nodes here
// keeps large enum temporaries out of frames held across recursive descent.
pub(super) type Expression = Box<SNode>;

#[cfg_attr(debug_assertions, inline(never))]
pub(super) fn expression(make: impl FnOnce() -> Node, span: Span) -> Expression {
    Box::new(spanned(make(), span))
}

#[cfg_attr(debug_assertions, inline(never))]
pub(super) fn binary(op: &str, left: Expression, right: Expression, span: Span) -> Expression {
    Box::new(spanned(
        Node::BinaryOp {
            op: op.into(),
            left,
            right,
        },
        span,
    ))
}

#[cfg_attr(debug_assertions, inline(never))]
pub(super) fn unary(op: &str, operand: Expression, span: Span) -> Expression {
    Box::new(spanned(
        Node::UnaryOp {
            op: op.into(),
            operand,
        },
        span,
    ))
}

#[cfg_attr(debug_assertions, inline(never))]
pub(super) fn ternary(
    condition: Expression,
    true_expr: Expression,
    false_expr: Expression,
    span: Span,
) -> Expression {
    Box::new(spanned(
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        },
        span,
    ))
}

#[cfg_attr(debug_assertions, inline(never))]
pub(super) fn range(start: Expression, end: Expression, inclusive: bool, span: Span) -> Expression {
    Box::new(spanned(
        Node::RangeExpr {
            start,
            end,
            inclusive,
        },
        span,
    ))
}
