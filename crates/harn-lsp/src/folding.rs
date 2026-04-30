use std::collections::HashSet;

use harn_lexer::{Lexer, Span, TokenKind};
use harn_parser::{MatchArm, Node, SNode};
use tower_lsp::lsp_types::{FoldingRange, FoldingRangeKind};

pub(crate) fn build_folding_ranges(source: &str, ast: Option<&[SNode]>) -> Vec<FoldingRange> {
    let mut ranges = Vec::new();
    let mut seen = HashSet::new();

    if let Some(program) = ast {
        for node in program {
            collect_ast_ranges(node, &mut ranges, &mut seen);
        }
    }

    collect_token_ranges(source, &mut ranges, &mut seen);
    ranges.sort_by_key(|range| {
        (
            range.start_line,
            range.start_character.unwrap_or(0),
            range.end_line,
        )
    });
    ranges
}

fn collect_token_ranges(
    source: &str,
    ranges: &mut Vec<FoldingRange>,
    seen: &mut HashSet<(u32, u32, u8)>,
) {
    let Ok(tokens) = Lexer::new(source).tokenize() else {
        return;
    };

    for token in tokens {
        let kind = match token.kind {
            TokenKind::BlockComment { .. } => Some(FoldingRangeKind::Comment),
            TokenKind::StringLiteral(_)
            | TokenKind::RawStringLiteral(_)
            | TokenKind::InterpolatedString(_) => Some(FoldingRangeKind::Region),
            _ => None,
        };
        let Some(kind) = kind else {
            continue;
        };
        push_span_range(ranges, seen, &token.span, Some(kind));
    }
}

fn collect_ast_ranges(
    node: &SNode,
    ranges: &mut Vec<FoldingRange>,
    seen: &mut HashSet<(u32, u32, u8)>,
) {
    match &node.node {
        Node::AttributedDecl { inner, .. } => collect_ast_ranges(inner, ranges, seen),
        Node::Pipeline { body, .. }
        | Node::OverrideDecl { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. }
        | Node::SpawnExpr { body }
        | Node::WhileLoop { body, .. }
        | Node::Retry { body, .. }
        | Node::TryExpr { body }
        | Node::DeferStmt { body }
        | Node::DeadlineBlock { body, .. }
        | Node::MutexBlock { body }
        | Node::Closure { body, .. } => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
            collect_body_ranges(body, ranges, seen);
        }
        Node::ImplBlock { methods, .. } => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
            collect_body_ranges(methods, ranges, seen);
        }
        Node::SkillDecl { fields, .. } => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
            for (_, value) in fields {
                collect_ast_ranges(value, ranges, seen);
            }
        }
        Node::EvalPackDecl {
            fields,
            body,
            summarize,
            ..
        } => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
            for (_, value) in fields {
                collect_ast_ranges(value, ranges, seen);
            }
            collect_body_ranges(body, ranges, seen);
            if let Some(summarize) = summarize {
                collect_body_ranges(summarize, ranges, seen);
            }
        }
        Node::EnumDecl { .. }
        | Node::StructDecl { .. }
        | Node::InterfaceDecl { .. }
        | Node::TypeDecl { .. } => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
        }
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
            collect_ast_ranges(condition, ranges, seen);
            collect_body_ranges(then_body, ranges, seen);
            if let Some(else_body) = else_body {
                collect_body_ranges(else_body, ranges, seen);
            }
        }
        Node::ForIn { iterable, body, .. } => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
            collect_ast_ranges(iterable, ranges, seen);
            collect_body_ranges(body, ranges, seen);
        }
        Node::MatchExpr { value, arms } => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
            collect_ast_ranges(value, ranges, seen);
            for arm in arms {
                collect_match_arm_ranges(arm, ranges, seen);
            }
        }
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
            collect_body_ranges(body, ranges, seen);
            collect_body_ranges(catch_body, ranges, seen);
            if let Some(finally_body) = finally_body {
                collect_body_ranges(finally_body, ranges, seen);
            }
        }
        Node::Parallel {
            expr,
            body,
            options,
            ..
        } => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
            collect_ast_ranges(expr, ranges, seen);
            collect_body_ranges(body, ranges, seen);
            for (_, value) in options {
                collect_ast_ranges(value, ranges, seen);
            }
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
            for case in cases {
                collect_ast_ranges(&case.channel, ranges, seen);
                collect_body_ranges(&case.body, ranges, seen);
            }
            if let Some((duration, body)) = timeout {
                collect_ast_ranges(duration, ranges, seen);
                collect_body_ranges(body, ranges, seen);
            }
            if let Some(body) = default_body {
                collect_body_ranges(body, ranges, seen);
            }
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
            collect_ast_ranges(condition, ranges, seen);
            collect_body_ranges(else_body, ranges, seen);
        }
        Node::RequireStmt { condition, message } => {
            collect_ast_ranges(condition, ranges, seen);
            if let Some(message) = message {
                collect_ast_ranges(message, ranges, seen);
            }
        }
        Node::FunctionCall { args, .. }
        | Node::EnumConstruct { args, .. }
        | Node::ListLiteral(args)
        | Node::OrPattern(args) => {
            for arg in args {
                collect_ast_ranges(arg, ranges, seen);
            }
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            collect_ast_ranges(object, ranges, seen);
            for arg in args {
                collect_ast_ranges(arg, ranges, seen);
            }
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            collect_ast_ranges(object, ranges, seen);
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            collect_ast_ranges(object, ranges, seen);
            collect_ast_ranges(index, ranges, seen);
        }
        Node::SliceAccess { object, start, end } => {
            collect_ast_ranges(object, ranges, seen);
            if let Some(start) = start {
                collect_ast_ranges(start, ranges, seen);
            }
            if let Some(end) = end {
                collect_ast_ranges(end, ranges, seen);
            }
        }
        Node::BinaryOp { left, right, .. }
        | Node::RangeExpr {
            start: left,
            end: right,
            ..
        } => {
            collect_ast_ranges(left, ranges, seen);
            collect_ast_ranges(right, ranges, seen);
        }
        Node::UnaryOp { operand, .. }
        | Node::TryOperator { operand }
        | Node::TryStar { operand }
        | Node::Spread(operand) => collect_ast_ranges(operand, ranges, seen),
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            collect_ast_ranges(condition, ranges, seen);
            collect_ast_ranges(true_expr, ranges, seen);
            collect_ast_ranges(false_expr, ranges, seen);
        }
        Node::Assignment { target, value, .. } => {
            collect_ast_ranges(target, ranges, seen);
            collect_ast_ranges(value, ranges, seen);
        }
        Node::LetBinding { value, .. }
        | Node::VarBinding { value, .. }
        | Node::ThrowStmt { value }
        | Node::EmitExpr { value } => collect_ast_ranges(value, ranges, seen),
        Node::ReturnStmt { value } | Node::YieldExpr { value } => {
            if let Some(value) = value {
                collect_ast_ranges(value, ranges, seen);
            }
        }
        Node::DictLiteral(entries)
        | Node::StructConstruct {
            fields: entries, ..
        } => {
            for entry in entries {
                collect_ast_ranges(&entry.key, ranges, seen);
                collect_ast_ranges(&entry.value, ranges, seen);
            }
        }
        Node::Block(stmts) => {
            push_span_range(ranges, seen, &node.span, Some(FoldingRangeKind::Region));
            collect_body_ranges(stmts, ranges, seen);
        }
        Node::ImportDecl { .. }
        | Node::SelectiveImport { .. }
        | Node::DurationLiteral(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::InterpolatedString(_)
        | Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral
        | Node::Identifier(_)
        | Node::BreakStmt
        | Node::ContinueStmt => {}
    }
}

fn collect_body_ranges(
    body: &[SNode],
    ranges: &mut Vec<FoldingRange>,
    seen: &mut HashSet<(u32, u32, u8)>,
) {
    for stmt in body {
        collect_ast_ranges(stmt, ranges, seen);
    }
}

fn collect_match_arm_ranges(
    arm: &MatchArm,
    ranges: &mut Vec<FoldingRange>,
    seen: &mut HashSet<(u32, u32, u8)>,
) {
    collect_ast_ranges(&arm.pattern, ranges, seen);
    if let Some(guard) = &arm.guard {
        collect_ast_ranges(guard, ranges, seen);
    }
    collect_body_ranges(&arm.body, ranges, seen);

    let Some(last) = arm.body.last() else {
        return;
    };
    let span = Span {
        start: arm.pattern.span.start,
        end: last.span.end,
        line: arm.pattern.span.line,
        column: arm.pattern.span.column,
        end_line: last.span.end_line,
    };
    push_span_range(ranges, seen, &span, Some(FoldingRangeKind::Region));
}

fn push_span_range(
    ranges: &mut Vec<FoldingRange>,
    seen: &mut HashSet<(u32, u32, u8)>,
    span: &Span,
    kind: Option<FoldingRangeKind>,
) {
    if span.line == 0 || span.end_line <= span.line {
        return;
    }
    let start_line = (span.line - 1) as u32;
    let end_line = (span.end_line - 1) as u32;
    let key = (start_line, end_line, folding_kind_key(kind.as_ref()));
    if !seen.insert(key) {
        return;
    }
    ranges.push(FoldingRange {
        start_line,
        start_character: Some(span.column.saturating_sub(1) as u32),
        end_line,
        end_character: None,
        kind,
        collapsed_text: None,
    });
}

fn folding_kind_key(kind: Option<&FoldingRangeKind>) -> u8 {
    match kind {
        Some(FoldingRangeKind::Comment) => 1,
        Some(FoldingRangeKind::Imports) => 2,
        Some(FoldingRangeKind::Region) => 3,
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::build_folding_ranges;
    use crate::document::DocumentState;
    use tower_lsp::lsp_types::FoldingRangeKind;

    #[test]
    fn builds_ranges_for_functions_strings_and_match_arms() {
        let source = concat!(
            "fn route(status) {\n",
            "  let prompt = \"\"\"\n",
            "    first\n",
            "    second\n",
            "  \"\"\"\n",
            "  match status {\n",
            "    \"ok\" -> {\n",
            "      log(prompt)\n",
            "      return true\n",
            "    }\n",
            "    _ -> { return false }\n",
            "  }\n",
            "}\n",
        );
        let state = DocumentState::new(source.to_string());
        let ranges = build_folding_ranges(source, state.cached_ast.as_deref());

        assert!(
            ranges
                .iter()
                .any(|range| range.start_line == 0 && range.end_line == 12),
            "expected function fold, got {ranges:?}"
        );
        assert!(
            ranges.iter().any(|range| {
                range.start_line == 1
                    && range.end_line == 4
                    && range.kind == Some(FoldingRangeKind::Region)
            }),
            "expected multiline string fold, got {ranges:?}"
        );
        assert!(
            ranges
                .iter()
                .any(|range| range.start_line == 6 && range.end_line == 8),
            "expected large match arm fold, got {ranges:?}"
        );
    }
}
