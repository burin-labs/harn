//! `unnecessary-parentheses` rule: parentheses around a single value
//! expression add noise and can hide places where the grammar now accepts
//! the direct form.

use std::collections::HashSet;

use harn_lexer::{FixEdit, Span, Token, TokenKind};
use harn_parser::{DiagnosticCode as Code, Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::rules::ast_walk::collect_expression_spans;

pub(crate) fn check_unnecessary_parentheses(
    source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let mut lexer = harn_lexer::Lexer::new(source);
    let Ok(tokens) = lexer.tokenize_with_comments() else {
        return;
    };

    let value_spans = collect_single_value_spans(program);
    if value_spans.is_empty() {
        return;
    }

    let mut paren_stack = Vec::new();
    for (idx, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::LParen => paren_stack.push(idx),
            TokenKind::RParen => {
                let Some(open_idx) = paren_stack.pop() else {
                    continue;
                };
                if let Some((open_span, close_span)) =
                    pair_wraps_single_value(source, &tokens, open_idx, idx, &value_spans)
                {
                    let open = &tokens[open_idx];
                    diagnostics.push(LintDiagnostic {
                        code: Code::LintUnnecessaryParentheses,
                        rule: "unnecessary-parentheses".into(),
                        message: "unnecessary parentheses around a single value".to_string(),
                        span: Span::merge(open.span, token.span),
                        severity: LintSeverity::Warning,
                        suggestion: Some("remove the parentheses".to_string()),
                        fix: Some(vec![
                            FixEdit {
                                span: open_span,
                                replacement: String::new(),
                            },
                            FixEdit {
                                span: close_span,
                                replacement: String::new(),
                            },
                        ]),
                    });
                }
            }
            _ => {}
        }
    }
}

fn pair_wraps_single_value(
    source: &str,
    tokens: &[Token],
    open_idx: usize,
    close_idx: usize,
    value_spans: &HashSet<(usize, usize)>,
) -> Option<(Span, Span)> {
    if opening_is_required_by_context(tokens, open_idx) {
        return None;
    }

    let open = &tokens[open_idx];
    let close = &tokens[close_idx];
    let inner_start = trim_ascii_whitespace_forward(source, open.span.end, close.span.start)?;
    let inner_end = trim_ascii_whitespace_backward(source, inner_start, close.span.start);

    value_spans.contains(&(inner_start, inner_end)).then(|| {
        (
            span_from_offsets(source, open.span.start, inner_start),
            span_from_offsets(source, inner_end, close.span.end),
        )
    })
}

fn opening_is_required_by_context(tokens: &[Token], open_idx: usize) -> bool {
    previous_meaningful_kind(tokens, open_idx).is_some_and(|kind| {
        matches!(
            kind,
            TokenKind::Identifier(_)
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::Gt
                | TokenKind::Fn
                | TokenKind::RequestApproval
                | TokenKind::DualControl
                | TokenKind::AskUser
                | TokenKind::EscalateTo
                // `mutex(resource) { ... }`: the parens delimit the lock's
                // resource key and are required grammar, not redundant grouping.
                | TokenKind::Mutex
        )
    })
}

fn previous_meaningful_kind(tokens: &[Token], before_idx: usize) -> Option<&TokenKind> {
    tokens[..before_idx]
        .iter()
        .rev()
        .find(|token| !is_trivia(&token.kind))
        .map(|token| &token.kind)
}

fn is_trivia(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Newline | TokenKind::LineComment { .. } | TokenKind::BlockComment { .. }
    )
}

fn trim_ascii_whitespace_forward(source: &str, start: usize, end: usize) -> Option<usize> {
    let mut pos = start;
    while pos < end && source.as_bytes()[pos].is_ascii_whitespace() {
        pos += 1;
    }
    (pos < end).then_some(pos)
}

fn trim_ascii_whitespace_backward(source: &str, start: usize, end: usize) -> usize {
    let mut pos = end;
    while pos > start && source.as_bytes()[pos - 1].is_ascii_whitespace() {
        pos -= 1;
    }
    pos
}

fn span_from_offsets(source: &str, start: usize, end: usize) -> Span {
    let line = source[..start].bytes().filter(|b| *b == b'\n').count() + 1;
    let line_start = source[..start].rfind('\n').map(|idx| idx + 1).unwrap_or(0);
    Span::with_offsets(start, end, line, start - line_start + 1)
}

fn collect_single_value_spans(program: &[SNode]) -> HashSet<(usize, usize)> {
    collect_expression_spans(program, is_single_value_expr)
}

fn is_single_value_expr(node: &Node) -> bool {
    matches!(
        node,
        Node::FunctionCall { .. }
            | Node::MethodCall { .. }
            | Node::OptionalMethodCall { .. }
            | Node::PropertyAccess { .. }
            | Node::OptionalPropertyAccess { .. }
            | Node::SubscriptAccess { .. }
            | Node::OptionalSubscriptAccess { .. }
            | Node::SliceAccess { .. }
            | Node::EnumConstruct { .. }
            | Node::StructConstruct { .. }
            | Node::HitlExpr { .. }
            | Node::ListLiteral(_)
            | Node::DictLiteral(_)
            | Node::TryOperator { .. }
            | Node::InterpolatedString(_)
            | Node::StringLiteral(_)
            | Node::RawStringLiteral(_)
            | Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::Identifier(_)
            | Node::DurationLiteral(_)
    )
}
