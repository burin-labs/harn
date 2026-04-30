//! Translate runtime schema dicts (and `schema_of(T)`) into `TypeExpr`.
//!
//! Used by:
//! - `schema_is(x, S)` flow refinement (intersect / subtract on the truthy /
//!   falsy branches),
//! - `schema_expect(x, S)` strict-types narrowing (re-assigns `x` to the
//!   schema type and clears the boundary-source flag),
//! - inferring `Schema<T>`-typed arguments at LLM-call sites.
//!
//! These helpers are *pure*: they only read from a `TypeScope` to resolve
//! identifiers (schema bindings + named type aliases) and never emit
//! diagnostics.

use crate::ast::*;

use super::scope::TypeScope;

pub(super) fn schema_type_expr_from_node(node: &SNode, scope: &TypeScope) -> Option<TypeExpr> {
    match &node.node {
        Node::Identifier(name) => {
            // Prefer schema bindings (runtime dicts), then fall back to a
            // declared `type` alias with the same name. This powers
            // `schema_is(x, T)` / `schema_expect(x, T)` narrowing when `T`
            // is a type alias rather than a schema-dict binding.
            if let Some(schema) = scope.get_schema_binding(name).cloned().flatten() {
                return Some(schema);
            }
            scope.resolve_type(name).cloned()
        }
        Node::DictLiteral(entries) => schema_type_expr_from_dict(entries, scope),
        // `schema_of(T)` is a runtime function that returns the JSON-Schema
        // dict for a type alias. When its result is passed into a position
        // typed `Schema<T>`, we resolve the underlying alias to its
        // TypeExpr so the generic binding can extract `T`.
        Node::FunctionCall { name, args, .. } if name == "schema_of" && args.len() == 1 => {
            if let Node::Identifier(alias) = &args[0].node {
                return scope.resolve_type(alias).cloned();
            }
            None
        }
        _ => None,
    }
}

pub(super) fn schema_type_expr_from_dict(
    entries: &[DictEntry],
    scope: &TypeScope,
) -> Option<TypeExpr> {
    let mut type_name: Option<String> = None;
    let mut properties: Option<&SNode> = None;
    let mut required: Option<Vec<String>> = None;
    let mut items: Option<&SNode> = None;
    let mut union: Option<&SNode> = None;
    let mut nullable = false;
    let mut additional_properties: Option<&SNode> = None;

    for entry in entries {
        let key = schema_entry_key(&entry.key)?;
        match key.as_str() {
            "type" => match &entry.value.node {
                Node::StringLiteral(text) | Node::RawStringLiteral(text) => {
                    type_name = Some(normalize_schema_type_name(text));
                }
                Node::ListLiteral(items_list) => {
                    let union_members = items_list
                        .iter()
                        .filter_map(|item| match &item.node {
                            Node::StringLiteral(text) | Node::RawStringLiteral(text) => {
                                Some(TypeExpr::Named(normalize_schema_type_name(text)))
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>();
                    if !union_members.is_empty() {
                        return Some(TypeExpr::Union(union_members));
                    }
                }
                _ => {}
            },
            "properties" => properties = Some(&entry.value),
            "required" => {
                required = schema_required_names(&entry.value);
            }
            "items" => items = Some(&entry.value),
            "union" | "oneOf" | "anyOf" => union = Some(&entry.value),
            "nullable" => {
                nullable = matches!(entry.value.node, Node::BoolLiteral(true));
            }
            "additional_properties" | "additionalProperties" => {
                additional_properties = Some(&entry.value);
            }
            _ => {}
        }
    }

    let mut schema_type = if let Some(union_node) = union {
        schema_union_type_expr(union_node, scope)?
    } else if let Some(properties_node) = properties {
        let property_entries = match &properties_node.node {
            Node::DictLiteral(entries) => entries,
            _ => return None,
        };
        let required_names = required.unwrap_or_default();
        let mut fields = Vec::new();
        for entry in property_entries {
            let field_name = schema_entry_key(&entry.key)?;
            let field_type = schema_type_expr_from_node(&entry.value, scope)?;
            fields.push(ShapeField {
                name: field_name.clone(),
                type_expr: field_type,
                optional: !required_names.contains(&field_name),
            });
        }
        TypeExpr::Shape(fields)
    } else if let Some(item_node) = items {
        TypeExpr::List(Box::new(schema_type_expr_from_node(item_node, scope)?))
    } else if let Some(type_name) = type_name {
        if type_name == "dict" {
            if let Some(extra_node) = additional_properties {
                let value_type = match &extra_node.node {
                    Node::BoolLiteral(_) => None,
                    _ => schema_type_expr_from_node(extra_node, scope),
                };
                if let Some(value_type) = value_type {
                    TypeExpr::DictType(
                        Box::new(TypeExpr::Named("string".into())),
                        Box::new(value_type),
                    )
                } else {
                    TypeExpr::Named(type_name)
                }
            } else {
                TypeExpr::Named(type_name)
            }
        } else {
            TypeExpr::Named(type_name)
        }
    } else {
        return None;
    };

    if nullable {
        schema_type = match schema_type {
            TypeExpr::Union(mut members) => {
                if !members
                    .iter()
                    .any(|member| matches!(member, TypeExpr::Named(name) if name == "nil"))
                {
                    members.push(TypeExpr::Named("nil".into()));
                }
                TypeExpr::Union(members)
            }
            other => TypeExpr::Union(vec![other, TypeExpr::Named("nil".into())]),
        };
    }

    Some(schema_type)
}

pub(super) fn schema_union_type_expr(node: &SNode, scope: &TypeScope) -> Option<TypeExpr> {
    let Node::ListLiteral(items) = &node.node else {
        return None;
    };
    let members = items
        .iter()
        .filter_map(|item| schema_type_expr_from_node(item, scope))
        .collect::<Vec<_>>();
    match members.len() {
        0 => None,
        1 => members.into_iter().next(),
        _ => Some(TypeExpr::Union(members)),
    }
}

pub(super) fn schema_required_names(node: &SNode) -> Option<Vec<String>> {
    let Node::ListLiteral(items) = &node.node else {
        return None;
    };
    Some(
        items
            .iter()
            .filter_map(|item| match &item.node {
                Node::StringLiteral(text) | Node::RawStringLiteral(text) => Some(text.clone()),
                Node::Identifier(text) => Some(text.clone()),
                _ => None,
            })
            .collect(),
    )
}

pub(super) fn schema_entry_key(node: &SNode) -> Option<String> {
    match &node.node {
        Node::Identifier(name) => Some(name.clone()),
        Node::StringLiteral(name) | Node::RawStringLiteral(name) => Some(name.clone()),
        _ => None,
    }
}

pub(super) fn normalize_schema_type_name(text: &str) -> String {
    match text {
        "object" => "dict".into(),
        "array" => "list".into(),
        "integer" => "int".into(),
        "number" => "float".into(),
        "boolean" => "bool".into(),
        "null" => "nil".into(),
        other => other.into(),
    }
}
