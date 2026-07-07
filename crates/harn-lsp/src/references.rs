use harn_lexer::{Lexer, Span, TokenKind};
use harn_parser::{Node, SNode};

use crate::symbols::binding_pattern_names;

/// Find all identifier references matching `target_name` in the AST.
///
/// Definition sites contribute their *whole declaration span* (the AST does
/// not keep a separate span for the declared name), so callers that need
/// exact positions must refine the result with
/// [`identifier_token_spans_within`].
pub(crate) fn find_references(program: &[SNode], target_name: &str) -> Vec<Span> {
    let mut refs = Vec::new();
    for snode in program {
        collect_references(snode, target_name, &mut refs);
    }
    refs
}

/// Refine raw AST reference spans down to the exact identifier tokens.
///
/// Re-lexes `source` and keeps each `name` identifier token that falls
/// inside one of `ref_spans`, deduplicated by offset. This is what makes
/// find-references and rename point at the identifier itself instead of
/// highlighting an entire `fn`/`pipeline`/`let` declaration.
pub(crate) fn identifier_token_spans_within(
    source: &str,
    name: &str,
    ref_spans: &[Span],
) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut seen_offsets = std::collections::HashSet::new();
    let mut lexer = Lexer::new(source);
    let Ok(tokens) = lexer.tokenize() else {
        return spans;
    };
    for token in &tokens {
        if let TokenKind::Identifier(ref token_name) = token.kind {
            if token_name == name
                && ref_spans
                    .iter()
                    .any(|rs| token.span.start >= rs.start && token.span.end <= rs.end)
                && seen_offsets.insert(token.span.start)
            {
                spans.push(token.span);
            }
        }
    }
    spans
}

fn collect_references(snode: &SNode, target_name: &str, refs: &mut Vec<Span>) {
    match &snode.node {
        Node::Identifier(name) if name == target_name => {
            refs.push(snode.span);
        }
        Node::FunctionCall { name, args, .. } => {
            if name == target_name {
                refs.push(snode.span);
            }
            for a in args {
                collect_references(a, target_name, refs);
            }
        }
        // For definitions, the name itself is a "reference" too
        Node::Pipeline {
            name, body, params, ..
        } => {
            if name == target_name {
                refs.push(snode.span);
            }
            for p in params {
                if p == target_name {
                    refs.push(snode.span);
                }
            }
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::FnDecl {
            name, params, body, ..
        } => {
            if name == target_name {
                refs.push(snode.span);
            }
            for p in params {
                if p.name == target_name {
                    refs.push(snode.span);
                }
            }
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::LetBinding { pattern, value, .. } | Node::ConstBinding { pattern, value, .. } => {
            if binding_pattern_names(pattern)
                .iter()
                .any(|n| n == target_name)
            {
                refs.push(snode.span);
            }
            collect_references(value, target_name, refs);
        }
        Node::ForIn {
            pattern,
            iterable,
            body,
        } => {
            if binding_pattern_names(pattern)
                .iter()
                .any(|n| n == target_name)
            {
                refs.push(snode.span);
            }
            collect_references(iterable, target_name, refs);
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            collect_references(condition, target_name, refs);
            for s in then_body {
                collect_references(s, target_name, refs);
            }
            if let Some(eb) = else_body {
                for s in eb {
                    collect_references(s, target_name, refs);
                }
            }
        }
        Node::WhileLoop { condition, body } => {
            collect_references(condition, target_name, refs);
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::Retry { count, body } => {
            collect_references(count, target_name, refs);
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::CostRoute { options, body } => {
            for (_, value) in options {
                collect_references(value, target_name, refs);
            }
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::TryCatch {
            has_catch: _,
            body,
            error_var,
            catch_body,
            finally_body,
            ..
        } => {
            for s in body {
                collect_references(s, target_name, refs);
            }
            if let Some(var) = error_var {
                if var == target_name {
                    refs.push(snode.span);
                }
            }
            for s in catch_body {
                collect_references(s, target_name, refs);
            }
            if let Some(fb) = finally_body {
                for s in fb {
                    collect_references(s, target_name, refs);
                }
            }
        }
        Node::TryExpr { body } => {
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::MatchExpr { value, arms } => {
            collect_references(value, target_name, refs);
            for arm in arms {
                collect_references(&arm.pattern, target_name, refs);
                for s in &arm.body {
                    collect_references(s, target_name, refs);
                }
            }
        }
        Node::BinaryOp { left, right, .. } => {
            collect_references(left, target_name, refs);
            collect_references(right, target_name, refs);
        }
        Node::UnaryOp { operand, .. } => {
            collect_references(operand, target_name, refs);
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            collect_references(object, target_name, refs);
            for a in args {
                collect_references(a, target_name, refs);
            }
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            collect_references(object, target_name, refs);
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            collect_references(object, target_name, refs);
            collect_references(index, target_name, refs);
        }
        Node::SliceAccess { object, start, end } => {
            collect_references(object, target_name, refs);
            if let Some(s) = start {
                collect_references(s, target_name, refs);
            }
            if let Some(e) = end {
                collect_references(e, target_name, refs);
            }
        }
        Node::Assignment { target, value, .. } => {
            collect_references(target, target_name, refs);
            collect_references(value, target_name, refs);
        }
        Node::ReturnStmt { value: Some(v) } => {
            collect_references(v, target_name, refs);
        }
        Node::ThrowStmt { value } => {
            collect_references(value, target_name, refs);
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            collect_references(condition, target_name, refs);
            collect_references(true_expr, target_name, refs);
            collect_references(false_expr, target_name, refs);
        }
        Node::Block(stmts)
        | Node::SpawnExpr { body: stmts }
        | Node::MutexBlock { body: stmts, .. }
        | Node::DeferStmt { body: stmts } => {
            for s in stmts {
                collect_references(s, target_name, refs);
            }
        }
        Node::Parallel {
            expr,
            variable,
            body,
            ..
        } => {
            collect_references(expr, target_name, refs);
            if let Some(var) = variable {
                if var == target_name {
                    refs.push(snode.span);
                }
            }
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::Closure { body, params, .. } => {
            for p in params {
                if p.name == target_name {
                    refs.push(snode.span);
                }
            }
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::DeadlineBlock { duration, body } => {
            collect_references(duration, target_name, refs);
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            collect_references(condition, target_name, refs);
            for s in else_body {
                collect_references(s, target_name, refs);
            }
        }
        Node::RangeExpr { start, end, .. } => {
            collect_references(start, target_name, refs);
            collect_references(end, target_name, refs);
        }
        Node::ListLiteral(items) => {
            for item in items {
                collect_references(item, target_name, refs);
            }
        }
        Node::DictLiteral(entries) => {
            for entry in entries {
                collect_references(&entry.key, target_name, refs);
                collect_references(&entry.value, target_name, refs);
            }
        }
        Node::StructConstruct { fields, .. } => {
            for entry in fields {
                collect_references(&entry.key, target_name, refs);
                collect_references(&entry.value, target_name, refs);
            }
        }
        Node::EnumConstruct { args, .. } => {
            for a in args {
                collect_references(a, target_name, refs);
            }
        }
        Node::OverrideDecl { name, body, .. } => {
            if name == target_name {
                refs.push(snode.span);
            }
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::YieldExpr { value: Some(v) } | Node::EmitExpr { value: v } => {
            collect_references(v, target_name, refs);
        }
        Node::EnumDecl { name, .. }
        | Node::StructDecl { name, .. }
        | Node::InterfaceDecl { name, .. }
            if name == target_name =>
        {
            refs.push(snode.span);
        }
        Node::AttributedDecl { inner, .. } => {
            collect_references(inner, target_name, refs);
        }
        // Terminals
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> Vec<SNode> {
        harn_parser::parse_source(source).expect("parse")
    }

    #[test]
    fn references_to_fn_param_refine_to_identifier_tokens() {
        let source = "fn process(data) {\n  return data\n}\n";
        let program = parse(source);
        let raw = find_references(&program, "data");
        // The declaration contributes the whole `fn` span; refinement must
        // shrink every hit to the 4-byte identifier itself.
        let refined = identifier_token_spans_within(source, "data", &raw);
        assert_eq!(refined.len(), 2, "param + body use; got {refined:?}");
        for span in &refined {
            assert_eq!(span.end - span.start, "data".len(), "span {span:?}");
            assert_eq!(&source[span.start..span.end], "data");
        }
    }

    #[test]
    fn references_to_let_binding_refine_to_identifier_tokens() {
        let source = "pipeline t(task) {\n  const total = 1\n  log(total)\n}\n";
        let program = parse(source);
        let raw = find_references(&program, "total");
        let refined = identifier_token_spans_within(source, "total", &raw);
        assert_eq!(refined.len(), 2, "binding + use; got {refined:?}");
        for span in &refined {
            assert_eq!(&source[span.start..span.end], "total");
        }
    }
}
