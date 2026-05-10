//! Generic AST visitor used by the linter, formatter, and any other
//! crate that needs to walk every `SNode` in a parsed program.
//!
//! Centralizing this here keeps a single source of truth for which
//! children each `Node` variant has — adding a new variant requires
//! one edit (in [`walk_children`]) and every consumer benefits.
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

use crate::ast::{BindingPattern, DictEntry, MatchArm, Node, SNode, SelectCase};

/// Walk every node in `program` in pre-order, invoking `visitor` on
/// each.
pub fn walk_program(program: &[SNode], visitor: &mut impl FnMut(&SNode)) {
    for node in program {
        walk_node(node, visitor);
    }
}

/// Visit `node`, then recurse into its children.
pub fn walk_node(node: &SNode, visitor: &mut impl FnMut(&SNode)) {
    visitor(node);
    walk_children(node, visitor);
}

/// Recurse into `node`'s children without re-visiting `node` itself.
/// Useful when a caller wants to handle the parent specially and then
/// continue the default traversal.
pub fn walk_children(node: &SNode, visitor: &mut impl FnMut(&SNode)) {
    match &node.node {
        Node::AttributedDecl { attributes, inner } => {
            for attr in attributes {
                for arg in &attr.args {
                    walk_node(&arg.value, visitor);
                }
            }
            walk_node(inner, visitor);
        }
        Node::Pipeline { body, .. } | Node::OverrideDecl { body, .. } => {
            walk_nodes(body, visitor);
        }
        Node::LetBinding { pattern, value, .. } | Node::VarBinding { pattern, value, .. } => {
            walk_binding_pattern(pattern, visitor);
            walk_node(value, visitor);
        }
        Node::EnumDecl { .. }
        | Node::StructDecl { .. }
        | Node::InterfaceDecl { .. }
        | Node::ImportDecl { .. }
        | Node::SelectiveImport { .. }
        | Node::TypeDecl { .. }
        | Node::BreakStmt
        | Node::ContinueStmt => {}
        Node::ImplBlock { methods, .. } => walk_nodes(methods, visitor),
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            walk_node(condition, visitor);
            walk_nodes(then_body, visitor);
            if let Some(body) = else_body {
                walk_nodes(body, visitor);
            }
        }
        Node::ForIn {
            pattern,
            iterable,
            body,
        } => {
            walk_binding_pattern(pattern, visitor);
            walk_node(iterable, visitor);
            walk_nodes(body, visitor);
        }
        Node::MatchExpr { value, arms } => {
            walk_node(value, visitor);
            for arm in arms {
                walk_match_arm(arm, visitor);
            }
        }
        Node::WhileLoop { condition, body } => {
            walk_node(condition, visitor);
            walk_nodes(body, visitor);
        }
        Node::Retry { count, body } => {
            walk_node(count, visitor);
            walk_nodes(body, visitor);
        }
        Node::CostRoute { options, body } => {
            walk_option_values(options, visitor);
            walk_nodes(body, visitor);
        }
        Node::ReturnStmt { value } | Node::YieldExpr { value } => {
            if let Some(value) = value {
                walk_node(value, visitor);
            }
        }
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            walk_nodes(body, visitor);
            walk_nodes(catch_body, visitor);
            if let Some(body) = finally_body {
                walk_nodes(body, visitor);
            }
        }
        Node::TryExpr { body }
        | Node::SpawnExpr { body }
        | Node::DeferStmt { body }
        | Node::MutexBlock { body }
        | Node::Block(body)
        | Node::Closure { body, .. } => walk_nodes(body, visitor),
        Node::FnDecl { body, .. } | Node::ToolDecl { body, .. } => {
            walk_nodes(body, visitor);
        }
        Node::SkillDecl { fields, .. } => walk_field_values(fields, visitor),
        Node::EvalPackDecl {
            fields,
            body,
            summarize,
            ..
        } => {
            walk_field_values(fields, visitor);
            walk_nodes(body, visitor);
            if let Some(body) = summarize {
                walk_nodes(body, visitor);
            }
        }
        Node::RangeExpr { start, end, .. } => {
            walk_node(start, visitor);
            walk_node(end, visitor);
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            walk_node(condition, visitor);
            walk_nodes(else_body, visitor);
        }
        Node::RequireStmt { condition, message } => {
            walk_node(condition, visitor);
            if let Some(message) = message {
                walk_node(message, visitor);
            }
        }
        Node::DeadlineBlock { duration, body } => {
            walk_node(duration, visitor);
            walk_nodes(body, visitor);
        }
        Node::EmitExpr { value }
        | Node::ThrowStmt { value }
        | Node::Spread(value)
        | Node::TryOperator { operand: value }
        | Node::TryStar { operand: value }
        | Node::UnaryOp { operand: value, .. } => walk_node(value, visitor),
        Node::HitlExpr { args, .. } => {
            for arg in args {
                walk_node(&arg.value, visitor);
            }
        }
        Node::Parallel {
            expr,
            body,
            options,
            ..
        } => {
            walk_node(expr, visitor);
            walk_option_values(options, visitor);
            walk_nodes(body, visitor);
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                walk_select_case(case, visitor);
            }
            if let Some((duration, body)) = timeout {
                walk_node(duration, visitor);
                walk_nodes(body, visitor);
            }
            if let Some(body) = default_body {
                walk_nodes(body, visitor);
            }
        }
        Node::FunctionCall { args, .. } | Node::EnumConstruct { args, .. } => {
            walk_nodes(args, visitor);
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            walk_node(object, visitor);
            walk_nodes(args, visitor);
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            walk_node(object, visitor);
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            walk_node(object, visitor);
            walk_node(index, visitor);
        }
        Node::SliceAccess { object, start, end } => {
            walk_node(object, visitor);
            if let Some(start) = start {
                walk_node(start, visitor);
            }
            if let Some(end) = end {
                walk_node(end, visitor);
            }
        }
        Node::BinaryOp { left, right, .. } => {
            walk_node(left, visitor);
            walk_node(right, visitor);
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            walk_node(condition, visitor);
            walk_node(true_expr, visitor);
            walk_node(false_expr, visitor);
        }
        Node::Assignment { target, value, .. } => {
            walk_node(target, visitor);
            walk_node(value, visitor);
        }
        Node::StructConstruct { fields, .. } | Node::DictLiteral(fields) => {
            walk_dict_entries(fields, visitor);
        }
        Node::ListLiteral(items) | Node::OrPattern(items) => walk_nodes(items, visitor),
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

fn walk_nodes(nodes: &[SNode], visitor: &mut impl FnMut(&SNode)) {
    for node in nodes {
        walk_node(node, visitor);
    }
}

fn walk_dict_entries(entries: &[DictEntry], visitor: &mut impl FnMut(&SNode)) {
    for entry in entries {
        walk_node(&entry.key, visitor);
        walk_node(&entry.value, visitor);
    }
}

fn walk_field_values(fields: &[(String, SNode)], visitor: &mut impl FnMut(&SNode)) {
    for (_, value) in fields {
        walk_node(value, visitor);
    }
}

fn walk_option_values(options: &[(String, SNode)], visitor: &mut impl FnMut(&SNode)) {
    for (_, value) in options {
        walk_node(value, visitor);
    }
}

fn walk_match_arm(arm: &MatchArm, visitor: &mut impl FnMut(&SNode)) {
    walk_node(&arm.pattern, visitor);
    if let Some(guard) = &arm.guard {
        walk_node(guard, visitor);
    }
    walk_nodes(&arm.body, visitor);
}

fn walk_select_case(case: &SelectCase, visitor: &mut impl FnMut(&SNode)) {
    walk_node(&case.channel, visitor);
    walk_nodes(&case.body, visitor);
}

fn walk_binding_pattern(pattern: &BindingPattern, visitor: &mut impl FnMut(&SNode)) {
    match pattern {
        BindingPattern::Identifier(_) | BindingPattern::Pair(_, _) => {}
        BindingPattern::Dict(fields) => {
            for field in fields {
                if let Some(default) = &field.default_value {
                    walk_node(default, visitor);
                }
            }
        }
        BindingPattern::List(items) => {
            for item in items {
                if let Some(default) = &item.default_value {
                    walk_node(default, visitor);
                }
            }
        }
    }
}
