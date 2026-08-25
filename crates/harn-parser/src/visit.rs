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

/// Walk `program` INCLUDING the expressions inside `${...}` string
/// interpolation.
///
/// [`walk_program`] cannot reach them: the lexer stores a hole as unparsed
/// source text plus a position, so `Node::InterpolatedString` is a leaf with no
/// AST children by construction. Any analysis that asks "is this name used?"
/// and walks with [`walk_program`] alone answers NO for a name used only inside
/// a hole — an unsound answer, not merely an incomplete one.
///
/// That is not hypothetical. The whole-program capability solver computed a
/// helper's required capabilities that way, concluded `random` was unused
/// because its only use was `"...${harness.random.uuid_v7()}"`, and deleted it
/// from the parameter type and every call site. The repair runs automatically
/// in the fleet bump workflow, so it rewrote working code into code that does
/// not compile (`value of type nil has no method uuid_v7`) and shipped it as a
/// bump PR (harn-cloud#1469).
///
/// `source` must be the whole file `program` was parsed from, so re-parsed
/// holes carry spans in the containing file's coordinates and stay safe to edit
/// against. A hole that fails to re-parse is skipped: it cannot be a well-typed
/// use, and the containing parse already reported it.
pub fn walk_program_interpolated(
    source: &str,
    program: &[SNode],
    visitor: &mut impl FnMut(&SNode),
) {
    let mut holes = Vec::new();
    walk_program(program, &mut |node| {
        if let Node::InterpolatedString(segments) = &node.node {
            for segment in segments {
                if let harn_lexer::StringSegment::Expression(text, line, column) = segment {
                    holes.push((text.clone(), *line, *column));
                }
            }
        }
        visitor(node);
    });
    // Recurse: an interpolation inside an interpolation is rare but legal, and
    // stopping at one level would restore the same unsoundness one layer down.
    while let Some((text, line, column)) = holes.pop() {
        let Some(expression) =
            crate::interpolation::parse_expression(Some(source), &text, line, column)
        else {
            continue;
        };
        walk_program_interpolated(source, std::slice::from_ref(&expression), visitor);
    }
}

/// Return whether a program contains a member access or method call whose
/// receiver is a bare identifier (`Name.Member`). This is the only syntax
/// whose lowering needs to distinguish an imported enum namespace from an
/// ordinary runtime object; callers can use the predicate to avoid resolving
/// the full import graph for files that cannot contain that ambiguity.
pub fn contains_identifier_receiver_access(program: &[SNode]) -> bool {
    any_node(program, &mut |node| {
        let object = match &node.node {
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::MethodCall { object, .. }
            | Node::OptionalMethodCall { object, .. } => object,
            _ => return false,
        };
        matches!(&object.node, Node::Identifier(_))
    })
}

/// Return whether a program contains an enum-shaped match pattern whose
/// receiver is a bare identifier (`Status.Ready` or `Status.Error(value)`).
///
/// Ordinary property access does not need enum metadata: the runtime module
/// namespace supplies the same value through normal property lookup. The
/// compiler only needs the imported-enum catalog when lowering these match
/// patterns, where a dotted expression is otherwise indistinguishable from a
/// value comparison. Keeping this predicate pattern-specific avoids forcing a
/// full import-graph walk on modules that merely use record or namespace
/// property access.
pub fn contains_identifier_enum_pattern(program: &[SNode]) -> bool {
    any_node(program, &mut |node| {
        let Node::MatchExpr { arms, .. } = &node.node else {
            return false;
        };
        arms.iter()
            .any(|arm| contains_identifier_enum_pattern_node(&arm.pattern))
    })
}

/// Pre-order walk that stops as soon as `predicate` returns true for a node.
/// The existence predicates above use this so a match near the top of a large
/// program does not pay for walking the rest of it.
fn any_node(program: &[SNode], predicate: &mut impl FnMut(&SNode) -> bool) -> bool {
    let mut stack = Vec::with_capacity(program.len());
    push_nodes_reversed(program, &mut stack);
    let mut scratch: Vec<&SNode> = Vec::new();
    while let Some(node) = stack.pop() {
        if predicate(node) {
            return true;
        }
        scratch.clear();
        collect_children(node, &mut |child| scratch.push(child));
        stack.extend(scratch.iter().rev().copied());
    }
    false
}

fn contains_identifier_enum_pattern_node(node: &SNode) -> bool {
    match &node.node {
        Node::PropertyAccess { object, .. } | Node::MethodCall { object, .. } => {
            matches!(&object.node, Node::Identifier(_))
        }
        Node::OrPattern(patterns) => patterns.iter().any(contains_identifier_enum_pattern_node),
        _ => false,
    }
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
    collect_children(node, &mut |child| stack.push(child));
    stack.reverse();
    walk_stack(&mut stack, visitor);
}

fn walk_stack<'a>(stack: &mut Vec<&'a SNode>, visitor: &mut impl FnMut(&SNode)) {
    // One scratch buffer reused across the whole walk: collecting children
    // into a fresh Vec per node made every walker pay one heap alloc/free
    // per AST node, which dominated whole-module analyses.
    let mut scratch: Vec<&'a SNode> = Vec::new();
    while let Some(node) = stack.pop() {
        visitor(node);
        scratch.clear();
        collect_children(node, &mut |child| scratch.push(child));
        stack.extend(scratch.iter().rev().copied());
    }
}

fn push_nodes_reversed<'a>(nodes: &'a [SNode], stack: &mut Vec<&'a SNode>) {
    stack.extend(nodes.iter().rev());
}

/// Collect `node`'s immediate children without recursing. Lets callers walk
/// selectively (e.g. stop descending at nested loops) while still relying on
/// this module's single source of truth for each variant's children.
pub fn immediate_children(node: &SNode) -> Vec<&SNode> {
    let mut children = Vec::new();
    collect_children(node, &mut |child| children.push(child));
    children
}

/// Invoke `f` on each of `node`'s immediate children in source order,
/// without materializing a `Vec`. Recursive walkers that visit children
/// in place should prefer this over [`immediate_children`].
pub fn for_each_immediate_child<'a>(node: &'a SNode, f: &mut impl FnMut(&'a SNode)) {
    collect_children(node, f);
}

fn collect_children<'a>(node: &'a SNode, children: &mut impl FnMut(&'a SNode)) {
    match &node.node {
        Node::AttributedDecl { attributes, inner } => {
            for attr in attributes {
                for arg in &attr.args {
                    children(&arg.value);
                }
            }
            children(inner);
        }
        Node::Pipeline { body, .. } | Node::OverrideDecl { body, .. } => {
            collect_nodes(body, children);
        }
        Node::LetBinding { pattern, value, .. } | Node::ConstBinding { pattern, value, .. } => {
            collect_binding_pattern(pattern, children);
            children(value);
        }
        Node::EnumDecl { variants, .. } => {
            for variant in variants {
                collect_typed_param_defaults(&variant.fields, children);
            }
        }
        Node::StructDecl { .. }
        | Node::ImportDecl { .. }
        | Node::SelectiveImport { .. }
        | Node::NamespaceImport { .. }
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
            children(condition);
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
            children(iterable);
            collect_nodes(body, children);
        }
        Node::MatchExpr { value, arms } => {
            children(value);
            for arm in arms {
                collect_match_arm(arm, children);
            }
        }
        Node::WhileLoop { condition, body } => {
            children(condition);
            collect_nodes(body, children);
        }
        Node::Retry { count, body } => {
            children(count);
            collect_nodes(body, children);
        }
        Node::CostRoute { options, body } => {
            collect_option_values(options, children);
            collect_nodes(body, children);
        }
        Node::ReturnStmt { value } | Node::YieldExpr { value } => {
            if let Some(value) = value {
                children(value);
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
                children(key);
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
            children(start);
            children(end);
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            children(condition);
            collect_nodes(else_body, children);
        }
        Node::RequireStmt { condition, message } => {
            children(condition);
            if let Some(message) = message {
                children(message);
            }
        }
        Node::DeadlineBlock { duration, body } => {
            children(duration);
            collect_nodes(body, children);
        }
        Node::EmitExpr { value }
        | Node::ThrowStmt { value }
        | Node::Spread(value)
        | Node::TryOperator { operand: value }
        | Node::TryStar { operand: value }
        | Node::NonNullAssert { operand: value }
        | Node::UnaryOp { operand: value, .. } => children(value),
        Node::Parallel {
            expr,
            body,
            options,
            ..
        } => {
            children(expr);
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
                children(duration);
                collect_nodes(body, children);
            }
            if let Some(body) = default_body {
                collect_nodes(body, children);
            }
        }
        Node::FunctionCall { args, .. } | Node::EnumConstruct { args, .. } => {
            collect_nodes(args, children);
        }
        Node::ValueCall { callee, args } => {
            children(callee);
            collect_nodes(args, children);
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            children(object);
            collect_nodes(args, children);
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            children(object);
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            children(object);
            children(index);
        }
        Node::SliceAccess { object, start, end } => {
            children(object);
            if let Some(start) = start {
                children(start);
            }
            if let Some(end) = end {
                children(end);
            }
        }
        Node::BinaryOp { left, right, .. } => {
            children(left);
            children(right);
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            children(condition);
            children(true_expr);
            children(false_expr);
        }
        Node::Assignment { target, value, .. } => {
            children(target);
            children(value);
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

fn collect_nodes<'a>(nodes: &'a [SNode], children: &mut impl FnMut(&'a SNode)) {
    for node in nodes {
        children(node);
    }
}

fn collect_dict_entries<'a>(entries: &'a [DictEntry], children: &mut impl FnMut(&'a SNode)) {
    for entry in entries {
        children(&entry.key);
        children(&entry.value);
    }
}

fn collect_field_values<'a>(fields: &'a [(String, SNode)], children: &mut impl FnMut(&'a SNode)) {
    for (_, value) in fields {
        children(value);
    }
}

fn collect_option_values<'a>(options: &'a [(String, SNode)], children: &mut impl FnMut(&'a SNode)) {
    for (_, value) in options {
        children(value);
    }
}

fn collect_typed_param_defaults<'a>(
    params: &'a [TypedParam],
    children: &mut impl FnMut(&'a SNode),
) {
    for param in params {
        if let Some(default) = &param.default_value {
            children(default);
        }
    }
}

fn collect_match_arm<'a>(arm: &'a MatchArm, children: &mut impl FnMut(&'a SNode)) {
    children(&arm.pattern);
    if let Some(guard) = &arm.guard {
        children(guard);
    }
    collect_nodes(&arm.body, children);
}

fn collect_select_case<'a>(case: &'a SelectCase, children: &mut impl FnMut(&'a SNode)) {
    children(&case.channel);
    collect_nodes(&case.body, children);
}

fn collect_binding_pattern<'a>(pattern: &'a BindingPattern, children: &mut impl FnMut(&'a SNode)) {
    match pattern {
        BindingPattern::Identifier(_) | BindingPattern::Pair(_, _) => {}
        BindingPattern::Dict(fields) => {
            for field in fields {
                if let Some(default) = &field.default_value {
                    children(default);
                }
            }
        }
        BindingPattern::List(items) => {
            for item in items {
                if let Some(default) = &item.default_value {
                    children(default);
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
    fn identifier_receiver_access_predicate_ignores_function_calls() {
        let plain = vec![dummy(Node::FunctionCall {
            name: "helper".to_string(),
            type_args: Vec::new(),
            args: Vec::new(),
        })];
        assert!(!contains_identifier_receiver_access(&plain));

        let qualified = vec![dummy(Node::PropertyAccess {
            object: Box::new(dummy(Node::Identifier("Status".to_string()))),
            property: "Ready".to_string(),
        })];
        assert!(contains_identifier_receiver_access(&qualified));
    }

    #[test]
    fn enum_pattern_predicate_ignores_ordinary_property_access() {
        let ordinary = vec![dummy(Node::PropertyAccess {
            object: Box::new(dummy(Node::Identifier("record".to_string()))),
            property: "field".to_string(),
        })];
        assert!(!contains_identifier_enum_pattern(&ordinary));

        let pattern = dummy(Node::PropertyAccess {
            object: Box::new(dummy(Node::Identifier("Status".to_string()))),
            property: "Ready".to_string(),
        });
        let match_expr = dummy(Node::MatchExpr {
            value: Box::new(dummy(Node::Identifier("value".to_string()))),
            arms: vec![MatchArm {
                pattern,
                guard: None,
                body: Vec::new(),
                span: Span::dummy(),
            }],
        });
        assert!(contains_identifier_enum_pattern(&[match_expr]));
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
                span: harn_lexer::Span::dummy(),
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
