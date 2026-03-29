use harn_lexer::Span;
use harn_parser::{Node, SNode};

use crate::symbols::binding_pattern_names;

// ---------------------------------------------------------------------------
// Reference collection (AST-based)
// ---------------------------------------------------------------------------

/// Find all identifier references matching `target_name` in the AST.
pub(crate) fn find_references(program: &[SNode], target_name: &str) -> Vec<Span> {
    let mut refs = Vec::new();
    for snode in program {
        collect_references(snode, target_name, &mut refs);
    }
    refs
}

fn collect_references(snode: &SNode, target_name: &str, refs: &mut Vec<Span>) {
    match &snode.node {
        Node::Identifier(name) if name == target_name => {
            refs.push(snode.span);
        }
        Node::FunctionCall { name, args } => {
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
        Node::LetBinding { pattern, value, .. } | Node::VarBinding { pattern, value, .. } => {
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
        Node::TryCatch {
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
        Node::SubscriptAccess { object, index } => {
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
        Node::Block(stmts) | Node::SpawnExpr { body: stmts } | Node::MutexBlock { body: stmts } => {
            for s in stmts {
                collect_references(s, target_name, refs);
            }
        }
        Node::Parallel { count, body, .. } => {
            collect_references(count, target_name, refs);
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::ParallelMap {
            list,
            body,
            variable,
        } => {
            collect_references(list, target_name, refs);
            if variable == target_name {
                refs.push(snode.span);
            }
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::Closure { body, params } => {
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
        Node::DictLiteral(entries) | Node::AskExpr { fields: entries } => {
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
        Node::YieldExpr { value: Some(v) } => {
            collect_references(v, target_name, refs);
        }
        Node::EnumDecl { name, .. }
        | Node::StructDecl { name, .. }
        | Node::InterfaceDecl { name, .. } => {
            if name == target_name {
                refs.push(snode.span);
            }
        }
        // Terminals
        _ => {}
    }
}
