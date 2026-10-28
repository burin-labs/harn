//! Lexical proof that a member use reaches its source-module owner.

use tree_sitter::Node;

use crate::ast::{
    name_resolution::{binding_defeaters_at, rust_declaration_is_unconditional},
    Language,
};

use super::text;

#[derive(Clone, Copy)]
enum MemberKind {
    Field,
    Method(ReceiverMode),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReceiverMode {
    Owned,
    Shared,
    Mutable,
}

pub(super) fn uses_resolve_to(
    item: Node<'_>,
    source: &str,
    declaration: Node<'_>,
    owner: &str,
) -> bool {
    if !binding_defeaters_at(item, source, Language::Rust).is_empty()
        || !rust_declaration_is_unconditional(declaration, source)
    {
        return false;
    }
    let Some(kind) = member_kind(declaration) else {
        return false;
    };
    let Some(name) = declaration.child_by_field_name("name") else {
        return false;
    };
    let member = text::text(name, source);
    let mut seen = false;
    let mut proven = true;
    text::walk(item, |node| {
        if matches!(
            node.kind(),
            "field_identifier" | "shorthand_field_identifier"
        ) && text::text(node, source) == member
        {
            seen = true;
            proven &= type_is_unshadowed(node, item, source, owner)
                && member_owner(node, item, source, kind) == Some(owner);
        }
        true
    });
    seen && proven
}

fn member_kind(declaration: Node<'_>) -> Option<MemberKind> {
    match declaration.kind() {
        "field_declaration" => Some(MemberKind::Field),
        "function_item" => {
            let parameters = declaration.child_by_field_name("parameters")?;
            let parameter = parameters.named_child(0)?;
            (parameter.kind() == "self_parameter")
                .then(|| MemberKind::Method(receiver_mode(parameter)))
        }
        _ => None,
    }
}

fn receiver_mode(node: Node<'_>) -> ReceiverMode {
    let mut cursor = node.walk();
    if !node.children(&mut cursor).any(|child| child.kind() == "&") {
        ReceiverMode::Owned
    } else if text::members(node)
        .into_iter()
        .any(|child| child.kind() == "mutable_specifier")
    {
        ReceiverMode::Mutable
    } else {
        ReceiverMode::Shared
    }
}

fn declared_receiver_type<'a>(ty: Node<'a>, source: &'a str, kind: MemberKind) -> Option<&'a str> {
    let MemberKind::Method(expected) = kind else {
        return base_type(ty, source);
    };
    // A different adjustment can select a trait method before the inherent one.
    if receiver_mode(ty) != expected {
        return None;
    }
    let nominal = if ty.kind() == "reference_type" {
        ty.child_by_field_name("type")?
    } else {
        ty
    };
    (nominal.kind() == "type_identifier").then(|| text::text(nominal, source))
}

fn base_type<'a>(mut ty: Node<'a>, source: &'a str) -> Option<&'a str> {
    loop {
        match ty.kind() {
            "reference_type" | "pointer_type" | "generic_type" => {
                ty = ty.child_by_field_name("type")?;
            }
            // A qualified type's terminal spelling does not establish its owner.
            "type_identifier" => return Some(text::text(ty, source)),
            _ => return None,
        }
    }
}

fn mentions(node: Node<'_>, source: &str, name: &str) -> bool {
    let mut found = false;
    text::walk(node, |node| {
        found |= matches!(node.kind(), "identifier" | "type_identifier")
            && text::text(node, source) == name;
        !found
    });
    found
}

fn type_is_unshadowed(node: Node<'_>, item: Node<'_>, source: &str, owner: &str) -> bool {
    let mut up = Some(node);
    while let Some(scope) = up {
        if scope
            .child_by_field_name("type_parameters")
            .is_some_and(|parameters| mentions(parameters, source, owner))
        {
            return false;
        }
        if scope.kind() == "mod_item" {
            return false;
        }
        if scope.kind() == "block"
            && text::members(scope).into_iter().any(|child| {
                (matches!(
                    child.kind(),
                    "struct_item" | "enum_item" | "union_item" | "type_item" | "trait_item"
                ) && child
                    .child_by_field_name("name")
                    .is_some_and(|name| text::text(name, source) == owner))
                    || (child.kind() == "use_declaration" && mentions(child, source, owner))
            })
        {
            return false;
        }
        if scope.id() == item.id() {
            break;
        }
        up = scope.parent();
    }
    true
}

fn member_owner<'a>(
    node: Node<'a>,
    item: Node<'a>,
    source: &'a str,
    kind: MemberKind,
) -> Option<&'a str> {
    let parent = node.parent()?;
    if parent.kind() == "field_expression" {
        let receiver = parent.child_by_field_name("value")?;
        return (receiver.kind() == "identifier")
            .then(|| parameter_type(receiver, item, source, kind))
            .flatten();
    }
    let mut up = Some(parent);
    while let Some(scope) = up {
        let field = match scope.kind() {
            "struct_expression" => Some("name"),
            "struct_pattern" => Some("type"),
            _ => None,
        };
        if let Some(field) = field {
            return base_type(scope.child_by_field_name(field)?, source);
        }
        if scope.id() == item.id() {
            break;
        }
        up = scope.parent();
    }
    None
}

fn parameter_type<'a>(
    receiver: Node<'a>,
    item: Node<'a>,
    source: &'a str,
    kind: MemberKind,
) -> Option<&'a str> {
    let name = text::text(receiver, source);
    let at = receiver.start_byte();
    let mut up = receiver.parent();
    while let Some(scope) = up {
        match scope.kind() {
            "block"
                if text::members(scope)
                    .into_iter()
                    .any(|child| match child.kind() {
                        "let_declaration" => {
                            child.end_byte() <= at
                                && child
                                    .child_by_field_name("pattern")
                                    .is_some_and(|pattern| mentions(pattern, source, name))
                        }
                        "function_item" | "const_item" | "static_item" | "struct_item" => child
                            .child_by_field_name("name")
                            .is_some_and(|binding| text::text(binding, source) == name),
                        "use_declaration" => mentions(child, source, name),
                        _ => false,
                    }) =>
            {
                return None;
            }
            "closure_expression"
                if scope
                    .child_by_field_name("parameters")
                    .is_some_and(|parameters| mentions(parameters, source, name)) =>
            {
                return None;
            }
            "for_expression" | "match_arm"
                if scope
                    .child_by_field_name("pattern")
                    .is_some_and(|pattern| mentions(pattern, source, name)) =>
            {
                return None;
            }
            "if_expression" | "while_expression" => {
                let mut shadows = false;
                if let Some(condition) = scope.child_by_field_name("condition") {
                    text::walk(condition, |node| {
                        shadows |= node.kind() == "let_condition"
                            && node
                                .child_by_field_name("pattern")
                                .is_some_and(|pattern| mentions(pattern, source, name));
                        !shadows
                    });
                }
                if shadows {
                    return None;
                }
            }
            "function_item" => {
                let parameters = scope.child_by_field_name("parameters")?;
                for parameter in text::members(parameters) {
                    if let Some(pattern) = parameter.child_by_field_name("pattern") {
                        if mentions(pattern, source, name) {
                            return (pattern.kind() == "identifier"
                                && text::text(pattern, source) == name)
                                .then(|| {
                                    parameter
                                        .child_by_field_name("type")
                                        .and_then(|ty| declared_receiver_type(ty, source, kind))
                                })
                                .flatten();
                        }
                    }
                }
                // A nested function never captures its parent's parameters.
                return None;
            }
            "mod_item" => return None,
            _ => {}
        }
        if scope.id() == item.id() {
            break;
        }
        up = scope.parent();
    }
    None
}
