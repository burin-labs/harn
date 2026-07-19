//! Generic AST visitor used by the linter, formatter, and any other
//! crate that needs to walk every `SNode` in a parsed program.
//!
//! Centralizing this here keeps a single source of truth for which
//! children each `Node` variant has — adding a new variant requires
//! one edit (in `collect_children`) and every consumer benefits.
//!
//! # Usage
//!
//! ```ignore
//! use harn_parser::visit::walk_program;
//! let mut count = 0;
//! walk_program(&program, &mut |node| {
//!     if matches!(&node.node, harn_parser::Node::FunctionCall { .. }) {
//!         count += 1;
//!     }
//! });
//! ```
//!
//! The visitor invokes the closure on each node *before* recursing
//! into its children (pre-order). To stop recursion at a particular
//! node, prefer using [`walk_children`] directly.

use crate::ast::{BindingPattern, DictEntry, MatchArm, Node, SNode, SelectCase, TypedParam};

/// Walk every node in `program` in pre-order, invoking `visitor` on
/// each.
pub fn walk_program(program: &[SNode], visitor: &mut impl FnMut(&SNode)) {
    let mut stack = Vec::with_capacity(program.len());
    push_nodes_reversed(program, &mut stack);
    walk_stack(&mut stack, visitor);
}

/// Visit `node`, then recurse into its children.
pub fn walk_node(node: &SNode, visitor: &mut impl FnMut(&SNode)) {
    let mut stack = vec![node];
    walk_stack(&mut stack, visitor);
}

/// Recurse into `node`'s children without re-visiting `node` itself.
/// Useful when a caller wants to handle the parent specially and then
/// continue the default traversal.
pub fn walk_children(node: &SNode, visitor: &mut impl FnMut(&SNode)) {
    let mut stack = Vec::new();
    push_children_reversed(node, &mut stack);
    walk_stack(&mut stack, visitor);
}

fn walk_stack(stack: &mut Vec<&SNode>, visitor: &mut impl FnMut(&SNode)) {
    while let Some(node) = stack.pop() {
        visitor(node);
        push_children_reversed(node, stack);
    }
}

fn push_children_reversed<'a>(node: &'a SNode, stack: &mut Vec<&'a SNode>) {
    let mut children = Vec::new();
    collect_children(node, &mut children);
    stack.extend(children.into_iter().rev());
}

fn push_nodes_reversed<'a>(nodes: &'a [SNode], stack: &mut Vec<&'a SNode>) {
    stack.extend(nodes.iter().rev());
}

/// Collect `node`'s immediate children without recursing. Lets callers walk
/// selectively (e.g. stop descending at nested loops) while still relying on
/// this module's single source of truth for each variant's children.
pub fn immediate_children(node: &SNode) -> Vec<&SNode> {
    let mut children = Vec::new();
    collect_children(node, &mut children);
    children
}

fn collect_children<'a>(node: &'a SNode, children: &mut Vec<&'a SNode>) {
    match &node.node {
        Node::AttributedDecl { attributes, inner } => {
            for attr in attributes {
                for arg in &attr.args {
                    children.push(&arg.value);
                }
            }
            children.push(inner);
        }
        Node::Pipeline { body, .. } | Node::OverrideDecl { body, .. } => {
            collect_nodes(body, children);
        }
        Node::LetBinding { pattern, value, .. } | Node::ConstBinding { pattern, value, .. } => {
            collect_binding_pattern(pattern, children);
            children.push(value);
        }
        Node::EnumDecl { variants, .. } => {
            for variant in variants {
                collect_typed_param_defaults(&variant.fields, children);
            }
        }
        Node::StructDecl { .. }
        | Node::ImportDecl { .. }
        | Node::SelectiveImport { .. }
        | Node::TypeDecl { .. }
        | Node::BreakStmt
        | Node::ContinueStmt => {}
        Node::InterfaceDecl { methods, .. } => {
            for method in methods {
                collect_typed_param_defaults(&method.params, children);
            }
        }
        Node::ImplBlock { methods, .. } => collect_nodes(methods, children),
        Node::IfElse {
            condition,
            then_body,
            else_body,
            ..
        } => {
            children.push(condition);
            collect_nodes(then_body, children);
            if let Some(body) = else_body {
                collect_nodes(body, children);
            }
        }
        Node::ForIn {
            pattern,
            iterable,
            body,
        } => {
            collect_binding_pattern(pattern, children);
            children.push(iterable);
            collect_nodes(body, children);
        }
        Node::MatchExpr { value, arms } => {
            children.push(value);
            for arm in arms {
                collect_match_arm(arm, children);
            }
        }
        Node::WhileLoop { condition, body } => {
            children.push(condition);
            collect_nodes(body, children);
        }
        Node::Retry { count, body } => {
            children.push(count);
            collect_nodes(body, children);
        }
        Node::CostRoute { options, body } => {
            collect_option_values(options, children);
            collect_nodes(body, children);
        }
        Node::ReturnStmt { value } | Node::YieldExpr { value } => {
            if let Some(value) = value {
                children.push(value);
            }
        }
        Node::TryCatch {
            has_catch: _,
            body,
            catch_body,
            finally_body,
            ..
        } => {
            collect_nodes(body, children);
            collect_nodes(catch_body, children);
            if let Some(body) = finally_body {
                collect_nodes(body, children);
            }
        }
        Node::TryExpr { body }
        | Node::SpawnExpr { body }
        | Node::ScopeBlock { body }
        | Node::DeferStmt { body }
        | Node::Block(body) => collect_nodes(body, children),
        Node::Closure { params, body, .. } => {
            collect_typed_param_defaults(params, children);
            collect_nodes(body, children);
        }
        Node::MutexBlock { key, body } => {
            if let Some(key) = key {
                children.push(key);
            }
            collect_nodes(body, children);
        }
        Node::FnDecl { params, body, .. } | Node::ToolDecl { params, body, .. } => {
            collect_typed_param_defaults(params, children);
            collect_nodes(body, children);
        }
        Node::SkillDecl { fields, .. } => collect_field_values(fields, children),
        Node::EvalPackDecl {
            fields,
            body,
            summarize,
            ..
        } => {
            collect_field_values(fields, children);
            collect_nodes(body, children);
            if let Some(body) = summarize {
                collect_nodes(body, children);
            }
        }
        Node::RangeExpr { start, end, .. } => {
            children.push(start);
            children.push(end);
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            children.push(condition);
            collect_nodes(else_body, children);
        }
        Node::RequireStmt { condition, message } => {
            children.push(condition);
            if let Some(message) = message {
                children.push(message);
            }
        }
        Node::DeadlineBlock { duration, body } => {
            children.push(duration);
            collect_nodes(body, children);
        }
        Node::EmitExpr { value }
        | Node::ThrowStmt { value }
        | Node::Spread(value)
        | Node::TryOperator { operand: value }
        | Node::TryStar { operand: value }
        | Node::NonNullAssert { operand: value }
        | Node::UnaryOp { operand: value, .. } => children.push(value),
        Node::HitlExpr { args, .. } => {
            for arg in args {
                children.push(&arg.value);
            }
        }
        Node::Parallel {
            expr,
            body,
            options,
            ..
        } => {
            children.push(expr);
            collect_option_values(options, children);
            collect_nodes(body, children);
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                collect_select_case(case, children);
            }
            if let Some((duration, body)) = timeout {
                children.push(duration);
                collect_nodes(body, children);
            }
            if let Some(body) = default_body {
                collect_nodes(body, children);
            }
        }
        Node::FunctionCall { args, .. } | Node::EnumConstruct { args, .. } => {
            collect_nodes(args, children);
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            children.push(object);
            collect_nodes(args, children);
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            children.push(object);
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            children.push(object);
            children.push(index);
        }
        Node::SliceAccess { object, start, end } => {
            children.push(object);
            if let Some(start) = start {
                children.push(start);
            }
            if let Some(end) = end {
                children.push(end);
            }
        }
        Node::BinaryOp { left, right, .. } => {
            children.push(left);
            children.push(right);
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            children.push(condition);
            children.push(true_expr);
            children.push(false_expr);
        }
        Node::Assignment { target, value, .. } => {
            children.push(target);
            children.push(value);
        }
        Node::StructConstruct { fields, .. } | Node::DictLiteral(fields) => {
            collect_dict_entries(fields, children);
        }
        Node::ListLiteral(items) | Node::OrPattern(items) => collect_nodes(items, children),
        Node::InterpolatedString(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral
        | Node::Identifier(_)
        | Node::DurationLiteral(_) => {}
    }
}

fn collect_nodes<'a>(nodes: &'a [SNode], children: &mut Vec<&'a SNode>) {
    children.extend(nodes.iter());
}

fn collect_dict_entries<'a>(entries: &'a [DictEntry], children: &mut Vec<&'a SNode>) {
    for entry in entries {
        children.push(&entry.key);
        children.push(&entry.value);
    }
}

fn collect_field_values<'a>(fields: &'a [(String, SNode)], children: &mut Vec<&'a SNode>) {
    for (_, value) in fields {
        children.push(value);
    }
}

fn collect_option_values<'a>(options: &'a [(String, SNode)], children: &mut Vec<&'a SNode>) {
    for (_, value) in options {
        children.push(value);
    }
}

fn collect_typed_param_defaults<'a>(params: &'a [TypedParam], children: &mut Vec<&'a SNode>) {
    for param in params {
        if let Some(default) = &param.default_value {
            children.push(default);
        }
    }
}

fn collect_match_arm<'a>(arm: &'a MatchArm, children: &mut Vec<&'a SNode>) {
    children.push(&arm.pattern);
    if let Some(guard) = &arm.guard {
        children.push(guard);
    }
    collect_nodes(&arm.body, children);
}

fn collect_select_case<'a>(case: &'a SelectCase, children: &mut Vec<&'a SNode>) {
    children.push(&case.channel);
    collect_nodes(&case.body, children);
}

fn collect_binding_pattern<'a>(pattern: &'a BindingPattern, children: &mut Vec<&'a SNode>) {
    match pattern {
        BindingPattern::Identifier(_) | BindingPattern::Pair(_, _) => {}
        BindingPattern::Dict(fields) => {
            for field in fields {
                if let Some(default) = &field.default_value {
                    children.push(default);
                }
            }
        }
        BindingPattern::List(items) => {
            for item in items {
                if let Some(default) = &item.default_value {
                    children.push(default);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{spanned, Node, TypedParam};
    use harn_lexer::Span;

    fn dummy(node: Node) -> SNode {
        spanned(node, Span::dummy())
    }

    #[test]
    fn walk_program_preserves_preorder() {
        let program = vec![dummy(Node::LetBinding {
            pattern: BindingPattern::Identifier("x".to_string()),
            type_ann: None,
            value: Box::new(dummy(Node::BinaryOp {
                op: "+".to_string(),
                left: Box::new(dummy(Node::IntLiteral(1))),
                right: Box::new(dummy(Node::IntLiteral(2))),
            })),
            is_pub: false,
        })];
        let mut seen = Vec::new();

        walk_program(&program, &mut |node| {
            seen.push(match &node.node {
                Node::LetBinding { .. } => "let",
                Node::BinaryOp { .. } => "binary",
                Node::IntLiteral(1) => "one",
                Node::IntLiteral(2) => "two",
                other => panic!("unexpected node {other:?}"),
            });
        });

        assert_eq!(seen, vec!["let", "binary", "one", "two"]);
    }

    #[test]
    fn walk_node_handles_deep_unary_chain_iteratively() {
        let mut node = dummy(Node::IntLiteral(0));
        for _ in 0..10_000 {
            node = dummy(Node::UnaryOp {
                op: "!".to_string(),
                operand: Box::new(node),
            });
        }

        let mut count = 0usize;
        walk_node(&node, &mut |_| count += 1);

        assert_eq!(count, 10_001);
    }

    #[test]
    fn walk_node_visits_typed_param_defaults() {
        let default = dummy(Node::Identifier("fallback".to_string()));
        let node = dummy(Node::FnDecl {
            name: "load".to_string(),
            type_params: Vec::new(),
            params: vec![TypedParam {
                name: "root".to_string(),
                type_expr: None,
                default_value: Some(Box::new(default)),
                rest: false,
            }],
            return_type: None,
            throws: None,
            where_clauses: Vec::new(),
            body: Vec::new(),
            is_pub: false,
            is_stream: false,
        });
        let mut seen = Vec::new();

        walk_node(&node, &mut |node| {
            if let Node::Identifier(name) = &node.node {
                seen.push(name.clone());
            }
        });

        assert_eq!(seen, vec!["fallback"]);
    }
}
