//! Replace an existing capability projection without shifting ordinary arguments.

use std::collections::{BTreeSet, HashMap};

use harn_builtin_meta::CapabilityId;
use harn_lexer::{FixEdit, Lexer, Span, TokenKind};
use harn_parser::lexical::{resolved_identifier_bindings, BindingId};
use harn_parser::{visit, Node, SNode, TypeExpr, TypedParam};

use super::capability_arguments::type_expr_carries_capability;

pub(super) enum ArgumentRepair {
    Missing,
    Preserve,
    Replace(FixEdit),
}

pub(super) fn existing_projection(
    source: &str,
    program: &[SNode],
    imported: &[SNode],
    span: Span,
    expected: &TypeExpr,
    actual: &TypeExpr,
) -> ArgumentRepair {
    if !type_expr_carries_capability(actual)
        || matches!(actual, TypeExpr::Named(name) if name == "Harness")
    {
        return ArgumentRepair::Missing;
    }
    let TypeExpr::Named(expected) = expected else {
        return ArgumentRepair::Missing;
    };
    let capability = CapabilityId::from_type_name(expected);
    if expected != "Harness" && capability.is_none() {
        return ArgumentRepair::Missing;
    }
    let mut resolved = HashMap::new();
    let mut roots = BTreeSet::new();
    let mut pick_shadowed = imported
        .iter()
        .any(|node| declaration(node).is_some_and(|(name, _)| name == "pick"));
    visit::walk_program(program, &mut |node| {
        pick_shadowed |= declaration(node).is_some_and(|(name, _)| name == "pick");
        if let Node::LetBinding { pattern, .. } | Node::ConstBinding { pattern, .. } = &node.node {
            pick_shadowed |= harn_parser::lexical::binding_pattern_names(pattern)
                .iter()
                .any(|name| name == "pick");
        }
        let (params, body) = match &node.node {
            Node::FnDecl { params, body, .. }
            | Node::ToolDecl { params, body, .. }
            | Node::Pipeline { params, body, .. }
            | Node::Closure { params, body, .. } => (params, body),
            _ => return,
        };
        if node.span.start <= span.start && node.span.end >= span.end {
            resolved.extend(resolved_identifier_bindings(params, body));
        }
        for param in params {
            pick_shadowed |= param.name == "pick";
            if matches!(param.type_expr.as_ref(), Some(TypeExpr::Named(name)) if name == "Harness")
            {
                roots.insert(BindingId::from_declaration(&param.name, param.span));
            }
        }
    });
    let mut decision = ArgumentRepair::Preserve;
    visit::walk_program(program, &mut |node| {
        let Node::FunctionCall { name, args, .. } = &node.node else {
            return;
        };
        let Some(argument) = args
            .iter()
            .find(|arg| arg.span.start == span.start && arg.span.end == span.end)
        else {
            return;
        };
        // Bare carriers belong to the signature migration: it can add the
        // missing grant to their caller and replace the identifier in place.
        if matches!(argument.node, Node::Identifier(_)) {
            decision = ArgumentRepair::Missing;
            return;
        }
        let Some(params) = call_parameters(program, imported, name, node.span, &resolved) else {
            return;
        };
        let required = params
            .iter()
            .take_while(|param| param.default_value.is_none() && !param.rest)
            .count();
        let prefix = params
            .iter()
            .take_while(|param| {
                param
                    .type_expr
                    .as_ref()
                    .is_some_and(type_expr_carries_capability)
            })
            .count();
        if args.len() < required.max(prefix) {
            decision = ArgumentRepair::Missing;
            return;
        }
        let Some(root) = projection_root(argument, &resolved, &roots, pick_shadowed) else {
            return;
        };
        let Some(text) = source.get(span.start..span.end) else {
            return;
        };
        let Ok(tokens) = Lexer::new(text).tokenize_with_comments() else {
            return;
        };
        if tokens.iter().any(|token| {
            matches!(
                token.kind,
                TokenKind::LineComment { .. } | TokenKind::BlockComment { .. }
            )
        }) {
            return;
        }
        decision = ArgumentRepair::Replace(FixEdit {
            span,
            replacement: capability.map_or_else(
                || root.name.clone(),
                |cap| format!("{}.{}", root.name, cap.field_name()),
            ),
        });
    });
    decision
}

fn declaration(node: &SNode) -> Option<(&str, &[TypedParam])> {
    match &node.node {
        Node::AttributedDecl { inner, .. } => declaration(inner),
        Node::FnDecl { name, params, .. }
        | Node::ToolDecl { name, params, .. }
        | Node::Pipeline { name, params, .. } => Some((name, params)),
        _ => None,
    }
}

fn call_parameters<'a>(
    program: &'a [SNode],
    imported: &'a [SNode],
    name: &str,
    span: Span,
    resolved: &HashMap<(usize, usize), BindingId>,
) -> Option<&'a [TypedParam]> {
    let binding = resolved.get(&(span.start, span.end));
    for node in program {
        if let Some((candidate, params)) = declaration(node) {
            if candidate == name
                && binding.is_none_or(|id| *id == BindingId::from_declaration(name, node.span))
            {
                return Some(params);
            }
        }
    }
    // A local value or callback shadows an imported function, so its name is
    // insufficient evidence for adopting the imported signature.
    if binding.is_some_and(|id| {
        !program.iter().any(|node| {
            matches!(
                node.node,
                Node::ImportDecl { .. } | Node::SelectiveImport { .. }
            ) && id.declaration_start == node.span.start
                && id.declaration_end == node.span.end
        })
    }) {
        return None;
    }
    imported.iter().find_map(|node| {
        let (candidate, params) = declaration(node)?;
        (candidate == name).then_some(params)
    })
}

fn projection_root<'a>(
    node: &SNode,
    resolved: &'a HashMap<(usize, usize), BindingId>,
    roots: &BTreeSet<BindingId>,
    pick_shadowed: bool,
) -> Option<&'a BindingId> {
    let root_binding = |node: &SNode| {
        if !matches!(node.node, Node::Identifier(_)) {
            return None;
        }
        resolved
            .get(&(node.span.start, node.span.end))
            .filter(|id| roots.contains(*id))
    };
    match &node.node {
        Node::PropertyAccess { object, property } => {
            CapabilityId::from_field_name(property)?;
            root_binding(object)
        }
        Node::DictLiteral(entries) if !entries.is_empty() => {
            let mut root = None;
            for entry in entries {
                let key = match &entry.key.node {
                    Node::Identifier(key)
                    | Node::StringLiteral(key)
                    | Node::RawStringLiteral(key) => key,
                    _ => return None,
                };
                let Node::PropertyAccess { object, property } = &entry.value.node else {
                    return None;
                };
                CapabilityId::from_field_name(key)?;
                if key != property {
                    return None;
                }
                let current = root_binding(object)?;
                if root.is_some_and(|prior| prior != current) {
                    return None;
                }
                root = Some(current);
            }
            root
        }
        Node::FunctionCall { name, args, .. }
            if name == "pick"
                && args.len() == 2
                && !resolved.contains_key(&(node.span.start, node.span.end))
                && !pick_shadowed =>
        {
            let Node::ListLiteral(keys) = &args[1].node else {
                return None;
            };
            if keys.is_empty()
                || !keys.iter().all(|key| {
                    matches!(&key.node, Node::StringLiteral(key) | Node::RawStringLiteral(key)
                    if CapabilityId::from_field_name(key).is_some())
                })
            {
                return None;
            }
            root_binding(&args[0])
        }
        _ => None,
    }
}
