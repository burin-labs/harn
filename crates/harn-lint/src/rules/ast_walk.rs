//! Shared AST traversal helpers for source-aware lint rules.

use std::collections::HashSet;

use harn_parser::{AttributeArg, BindingPattern, DictEntry, MatchArm, Node, SNode, SelectCase};

pub(crate) fn collect_expression_spans(
    program: &[SNode],
    mut include: impl FnMut(&Node) -> bool,
) -> HashSet<(usize, usize)> {
    let mut spans = HashSet::new();
    for node in program {
        visit_node(node, &mut spans, &mut include);
    }
    spans
}

fn visit_node(
    node: &SNode,
    spans: &mut HashSet<(usize, usize)>,
    include: &mut impl FnMut(&Node) -> bool,
) {
    if include(&node.node) {
        spans.insert((node.span.start, node.span.end));
    }

    match &node.node {
        Node::AttributedDecl { attributes, inner } => {
            for attr in attributes {
                for arg in &attr.args {
                    visit_attribute_arg(arg, spans, include);
                }
            }
            visit_node(inner, spans, include);
        }
        Node::Pipeline { body, .. } | Node::OverrideDecl { body, .. } => {
            visit_nodes(body, spans, include);
        }
        Node::LetBinding { pattern, value, .. } | Node::VarBinding { pattern, value, .. } => {
            visit_binding_pattern(pattern, spans, include);
            visit_node(value, spans, include);
        }
        Node::EnumDecl { .. }
        | Node::StructDecl { .. }
        | Node::InterfaceDecl { .. }
        | Node::ImportDecl { .. }
        | Node::SelectiveImport { .. }
        | Node::TypeDecl { .. }
        | Node::BreakStmt
        | Node::ContinueStmt => {}
        Node::ImplBlock { methods, .. } => visit_nodes(methods, spans, include),
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            visit_node(condition, spans, include);
            visit_nodes(then_body, spans, include);
            if let Some(body) = else_body {
                visit_nodes(body, spans, include);
            }
        }
        Node::ForIn {
            pattern,
            iterable,
            body,
        } => {
            visit_binding_pattern(pattern, spans, include);
            visit_node(iterable, spans, include);
            visit_nodes(body, spans, include);
        }
        Node::MatchExpr { value, arms } => {
            visit_node(value, spans, include);
            for arm in arms {
                visit_match_arm(arm, spans, include);
            }
        }
        Node::WhileLoop { condition, body } => {
            visit_node(condition, spans, include);
            visit_nodes(body, spans, include);
        }
        Node::Retry { count, body } => {
            visit_node(count, spans, include);
            visit_nodes(body, spans, include);
        }
        Node::CostRoute { options, body } => {
            visit_option_values(options, spans, include);
            visit_nodes(body, spans, include);
        }
        Node::ReturnStmt { value } | Node::YieldExpr { value } => {
            if let Some(value) = value {
                visit_node(value, spans, include);
            }
        }
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            visit_nodes(body, spans, include);
            visit_nodes(catch_body, spans, include);
            if let Some(body) = finally_body {
                visit_nodes(body, spans, include);
            }
        }
        Node::TryExpr { body }
        | Node::SpawnExpr { body }
        | Node::DeferStmt { body }
        | Node::MutexBlock { body }
        | Node::Block(body)
        | Node::Closure { body, .. } => visit_nodes(body, spans, include),
        Node::FnDecl { body, .. } | Node::ToolDecl { body, .. } => {
            visit_nodes(body, spans, include);
        }
        Node::SkillDecl { fields, .. } => visit_field_values(fields, spans, include),
        Node::EvalPackDecl {
            fields,
            body,
            summarize,
            ..
        } => {
            visit_field_values(fields, spans, include);
            visit_nodes(body, spans, include);
            if let Some(body) = summarize {
                visit_nodes(body, spans, include);
            }
        }
        Node::RangeExpr { start, end, .. } => {
            visit_node(start, spans, include);
            visit_node(end, spans, include);
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            visit_node(condition, spans, include);
            visit_nodes(else_body, spans, include);
        }
        Node::RequireStmt { condition, message } => {
            visit_node(condition, spans, include);
            if let Some(message) = message {
                visit_node(message, spans, include);
            }
        }
        Node::DeadlineBlock { duration, body } => {
            visit_node(duration, spans, include);
            visit_nodes(body, spans, include);
        }
        Node::EmitExpr { value }
        | Node::ThrowStmt { value }
        | Node::Spread(value)
        | Node::TryOperator { operand: value }
        | Node::TryStar { operand: value }
        | Node::UnaryOp { operand: value, .. } => visit_node(value, spans, include),
        Node::HitlExpr { args, .. } => {
            for arg in args {
                visit_node(&arg.value, spans, include);
            }
        }
        Node::Parallel {
            expr,
            body,
            options,
            ..
        } => {
            visit_node(expr, spans, include);
            visit_option_values(options, spans, include);
            visit_nodes(body, spans, include);
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                visit_select_case(case, spans, include);
            }
            if let Some((duration, body)) = timeout {
                visit_node(duration, spans, include);
                visit_nodes(body, spans, include);
            }
            if let Some(body) = default_body {
                visit_nodes(body, spans, include);
            }
        }
        Node::FunctionCall { args, .. } | Node::EnumConstruct { args, .. } => {
            visit_nodes(args, spans, include);
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            visit_node(object, spans, include);
            visit_nodes(args, spans, include);
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            visit_node(object, spans, include);
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            visit_node(object, spans, include);
            visit_node(index, spans, include);
        }
        Node::SliceAccess { object, start, end } => {
            visit_node(object, spans, include);
            if let Some(start) = start {
                visit_node(start, spans, include);
            }
            if let Some(end) = end {
                visit_node(end, spans, include);
            }
        }
        Node::BinaryOp { left, right, .. } => {
            visit_node(left, spans, include);
            visit_node(right, spans, include);
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            visit_node(condition, spans, include);
            visit_node(true_expr, spans, include);
            visit_node(false_expr, spans, include);
        }
        Node::Assignment { target, value, .. } => {
            visit_node(target, spans, include);
            visit_node(value, spans, include);
        }
        Node::StructConstruct { fields, .. } | Node::DictLiteral(fields) => {
            visit_dict_entries(fields, spans, include);
        }
        Node::ListLiteral(items) | Node::OrPattern(items) => visit_nodes(items, spans, include),
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

fn visit_nodes(
    nodes: &[SNode],
    spans: &mut HashSet<(usize, usize)>,
    include: &mut impl FnMut(&Node) -> bool,
) {
    for node in nodes {
        visit_node(node, spans, include);
    }
}

fn visit_attribute_arg(
    arg: &AttributeArg,
    spans: &mut HashSet<(usize, usize)>,
    include: &mut impl FnMut(&Node) -> bool,
) {
    visit_node(&arg.value, spans, include);
}

fn visit_dict_entries(
    entries: &[DictEntry],
    spans: &mut HashSet<(usize, usize)>,
    include: &mut impl FnMut(&Node) -> bool,
) {
    for entry in entries {
        visit_node(&entry.key, spans, include);
        visit_node(&entry.value, spans, include);
    }
}

fn visit_field_values(
    fields: &[(String, SNode)],
    spans: &mut HashSet<(usize, usize)>,
    include: &mut impl FnMut(&Node) -> bool,
) {
    for (_, value) in fields {
        visit_node(value, spans, include);
    }
}

fn visit_option_values(
    options: &[(String, SNode)],
    spans: &mut HashSet<(usize, usize)>,
    include: &mut impl FnMut(&Node) -> bool,
) {
    for (_, value) in options {
        visit_node(value, spans, include);
    }
}

fn visit_match_arm(
    arm: &MatchArm,
    spans: &mut HashSet<(usize, usize)>,
    include: &mut impl FnMut(&Node) -> bool,
) {
    visit_node(&arm.pattern, spans, include);
    if let Some(guard) = &arm.guard {
        visit_node(guard, spans, include);
    }
    visit_nodes(&arm.body, spans, include);
}

fn visit_select_case(
    case: &SelectCase,
    spans: &mut HashSet<(usize, usize)>,
    include: &mut impl FnMut(&Node) -> bool,
) {
    visit_node(&case.channel, spans, include);
    visit_nodes(&case.body, spans, include);
}

fn visit_binding_pattern(
    pattern: &BindingPattern,
    spans: &mut HashSet<(usize, usize)>,
    include: &mut impl FnMut(&Node) -> bool,
) {
    match pattern {
        BindingPattern::Identifier(_) | BindingPattern::Pair(_, _) => {}
        BindingPattern::Dict(fields) => {
            for field in fields {
                if let Some(default) = &field.default_value {
                    visit_node(default, spans, include);
                }
            }
        }
        BindingPattern::List(items) => {
            for item in items {
                if let Some(default) = &item.default_value {
                    visit_node(default, spans, include);
                }
            }
        }
    }
}
