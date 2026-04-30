//! Autofix helpers shared across lint rules. Each function operates on
//! AST nodes and raw source so it can be unit-tested without `Linter`
//! state.

use harn_lexer::{FixEdit, Span};
use harn_parser::{Node, SNode};

/// Rename a simple `let`/`var` binding's identifier with an underscore
/// prefix. Returns `None` for destructuring patterns, unusual formatting,
/// or anything else where the rewrite is not unambiguously safe.
pub(crate) fn simple_ident_rename_fix(
    source: Option<&str>,
    span: Span,
    name: &str,
) -> Option<Vec<FixEdit>> {
    let src = source?;
    let region = src.get(span.start..span.end)?;

    // Bail when there is no `let`/`var` keyword (e.g. a `for`-loop head).
    let (keyword_len, after_keyword_is_boundary) = if region.starts_with("let") {
        (
            3,
            region.as_bytes().get(3).is_some_and(|b| !is_ident_byte(*b)),
        )
    } else if region.starts_with("var") {
        (
            3,
            region.as_bytes().get(3).is_some_and(|b| !is_ident_byte(*b)),
        )
    } else {
        return None;
    };
    if !after_keyword_is_boundary {
        return None;
    }

    let mut cursor = keyword_len;
    let bytes = region.as_bytes();
    while cursor < bytes.len() && matches!(bytes[cursor], b' ' | b'\t') {
        cursor += 1;
    }

    // Identifier must match `name` exactly with a word boundary after it.
    let name_bytes = name.as_bytes();
    if region.get(cursor..cursor + name_bytes.len())?.as_bytes() != name_bytes {
        return None;
    }
    let trailing_ok = bytes
        .get(cursor + name_bytes.len())
        .is_none_or(|b| !is_ident_byte(*b));
    if !trailing_ok {
        return None;
    }

    let ident_start = span.start + cursor;
    let ident_end = ident_start + name_bytes.len();
    let replacement = format!("_{name}");
    Some(vec![FixEdit {
        span: Span::with_offsets(ident_start, ident_end, span.line, span.column + cursor),
        replacement,
    }])
}

/// Replace an unnecessary conversion call (e.g. `to_string("hi")`) with
/// the inner expression's source text. The outer call's span is replaced
/// verbatim with the inner span's source slice, so formatting and any
/// internal whitespace within the argument are preserved.
pub(crate) fn unnecessary_cast_fix(
    source: Option<&str>,
    call_span: Span,
    inner_span: Span,
) -> Option<Vec<FixEdit>> {
    let src = source?;
    let inner_text = src.get(inner_span.start..inner_span.end)?;
    Some(vec![FixEdit {
        span: call_span,
        replacement: inner_text.to_string(),
    }])
}

/// Replace an outer method call expression with its receiver text. Used for
/// syntactic wrappers like `value.clone()` where removing the method call is
/// the whole fix.
pub(crate) fn remove_method_call_wrapper_fix(
    source: Option<&str>,
    call_span: Span,
    receiver_span: Span,
) -> Option<Vec<FixEdit>> {
    let src = source?;
    let receiver_text = src.get(receiver_span.start..receiver_span.end)?;
    Some(vec![FixEdit {
        span: call_span,
        replacement: receiver_text.to_string(),
    }])
}

/// Append a sink call (`.to_list()` / `.to_set()` / `.to_dict()`) to the
/// end of an expression span.
pub(crate) fn append_sink_fix(expr_span: Span, sink: &str) -> Vec<FixEdit> {
    let replacement = format!(".{sink}()");
    vec![FixEdit {
        span: Span::with_offsets(
            expr_span.end,
            expr_span.end,
            expr_span.line,
            expr_span.column,
        ),
        replacement,
    }]
}

pub(crate) fn is_ident_byte(b: u8) -> bool {
    matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_')
}

/// Match a ternary of the form `x == nil ? fallback : x` or
/// `x != nil ? x : fallback` and return `(identifier, fallback_node)` when
/// the shape matches. `x` must be a bare identifier on both sides so the
/// rewrite to `x ?? fallback` is safe against double-evaluation.
pub(crate) fn nil_fallback_ternary_parts<'a>(
    condition: &'a SNode,
    true_expr: &'a SNode,
    false_expr: &'a SNode,
) -> Option<(String, &'a SNode)> {
    let Node::BinaryOp { op, left, right } = &condition.node else {
        return None;
    };
    let is_nil = |n: &Node| matches!(n, Node::NilLiteral);
    let identifier = |n: &Node| match n {
        Node::Identifier(name) => Some(name.clone()),
        _ => None,
    };

    let (ident_name, expected_non_nil_arm): (String, Which) = match op.as_str() {
        "==" => {
            if is_nil(&right.node) {
                (identifier(&left.node)?, Which::False)
            } else if is_nil(&left.node) {
                (identifier(&right.node)?, Which::False)
            } else {
                return None;
            }
        }
        "!=" => {
            if is_nil(&right.node) {
                (identifier(&left.node)?, Which::True)
            } else if is_nil(&left.node) {
                (identifier(&right.node)?, Which::True)
            } else {
                return None;
            }
        }
        _ => return None,
    };

    let (non_nil_arm, fallback_arm) = match expected_non_nil_arm {
        Which::True => (true_expr, false_expr),
        Which::False => (false_expr, true_expr),
    };
    match &non_nil_arm.node {
        Node::Identifier(name) if name == &ident_name => Some((ident_name, fallback_arm)),
        _ => None,
    }
}

pub(crate) enum Which {
    True,
    False,
}

/// Conservative side-effect analysis for lint autofixes. Returns true only
/// when the expression has no observable effect; anything we cannot prove
/// pure returns false so the autofix never silently drops a side effect.
pub(crate) fn is_pure_expression(node: &Node) -> bool {
    match node {
        Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::BoolLiteral(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::NilLiteral
        | Node::DurationLiteral(_)
        | Node::Identifier(_)
        | Node::BreakStmt
        | Node::ContinueStmt => true,
        Node::ListLiteral(items) => items.iter().all(|n| is_pure_expression(&n.node)),
        Node::DictLiteral(entries) => entries
            .iter()
            .all(|e| is_pure_expression(&e.key.node) && is_pure_expression(&e.value.node)),
        Node::UnaryOp { operand, .. } => is_pure_expression(&operand.node),
        Node::BinaryOp { left, right, .. } => {
            is_pure_expression(&left.node) && is_pure_expression(&right.node)
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            is_pure_expression(&condition.node)
                && is_pure_expression(&true_expr.node)
                && is_pure_expression(&false_expr.node)
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            is_pure_expression(&object.node)
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            is_pure_expression(&object.node) && is_pure_expression(&index.node)
        }
        Node::SliceAccess { object, start, end } => {
            is_pure_expression(&object.node)
                && start.as_deref().is_none_or(|n| is_pure_expression(&n.node))
                && end.as_deref().is_none_or(|n| is_pure_expression(&n.node))
        }
        Node::RangeExpr { start, end, .. } => {
            is_pure_expression(&start.node) && is_pure_expression(&end.node)
        }
        _ => false,
    }
}

/// Remove a whole statement including its leading indent and trailing
/// newline. Returns `None` when the rewrite cannot be performed safely,
/// in which case the lint still fires without an autofix.
pub(crate) fn empty_statement_removal_fix(
    source: Option<&str>,
    span: Span,
) -> Option<Vec<FixEdit>> {
    let src = source?;
    if span.start > src.len() || span.end > src.len() {
        return None;
    }

    // Swallow leading spaces/tabs so the indent goes with the statement.
    let mut start = span.start;
    let bytes = src.as_bytes();
    while start > 0 {
        let prev = bytes[start - 1];
        if prev == b' ' || prev == b'\t' {
            start -= 1;
            continue;
        }
        break;
    }

    // Swallow exactly one trailing newline — more would eat blank lines
    // that follow the removed statement.
    let mut end = span.end;
    if bytes.get(end) == Some(&b'\n') {
        end += 1;
    } else if bytes.get(end) == Some(&b'\r') && bytes.get(end + 1) == Some(&b'\n') {
        end += 2;
    }

    Some(vec![FixEdit {
        span: Span::with_offsets(start, end, span.line, 1),
        replacement: String::new(),
    }])
}
