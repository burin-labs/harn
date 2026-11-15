//! `HARN-LNT-080`: a `?.` chain over an untyped value.

use crate::ast::*;
use crate::diagnostic_codes::Code;

use super::super::scope::TypeScope;
use super::super::{is_gradual_type_name, TypeChecker};

const RULE: &str = "untyped-optional-chain";

fn optional_field_name(node: &SNode) -> Option<&str> {
    match &node.node {
        Node::OptionalPropertyAccess { property, .. } => Some(property),
        Node::OptionalSubscriptAccess { index, .. } => TypeChecker::literal_index_key(index),
        _ => None,
    }
}

fn optional_receiver(node: &SNode) -> Option<&SNode> {
    match &node.node {
        Node::OptionalPropertyAccess { object, .. }
        | Node::OptionalSubscriptAccess { object, .. } => Some(object),
        _ => None,
    }
}

impl TypeChecker {
    /// Report the first pair of optional links that crosses an untyped field.
    ///
    /// `data?.repository?.pullRequest` hedges every field because nothing
    /// declared the shape. Decoding once with `schema_parse(value,
    /// schema_of(T))` validates it and leaves typed fields behind. A chain
    /// over a typed record with optional fields is real nil handling and is
    /// left alone. Later untyped links do not repeat the same diagnostic.
    pub(in crate::typechecker) fn check_untyped_optional_chain(
        &mut self,
        snode: &SNode,
        scope: &TypeScope,
    ) {
        let mut links = Vec::new();
        let mut cursor = snode;
        loop {
            match &cursor.node {
                Node::OptionalPropertyAccess { object, .. }
                | Node::OptionalSubscriptAccess { object, .. } => {
                    links.push(cursor);
                    cursor = object;
                }
                Node::PropertyAccess { object, .. } | Node::SubscriptAccess { object, .. } => {
                    cursor = object;
                }
                _ => break,
            }
        }
        if links.len() < 2 {
            return;
        }
        links.reverse();
        for (index, pair) in links.windows(2).enumerate() {
            let Some(receiver) = optional_receiver(pair[0]) else {
                return;
            };
            let receiver_typed = self.infer_type(receiver, scope).is_some_and(|ty| {
                !self.type_is_untyped_record(&ty, optional_field_name(pair[0]), scope)
            });
            let Some(next_receiver) = optional_receiver(pair[1]) else {
                return;
            };
            let first_link_typed = self.infer_type(next_receiver, scope).is_some_and(|ty| {
                !self.type_is_untyped_record(&ty, optional_field_name(pair[1]), scope)
            });
            if !receiver_typed || !first_link_typed {
                // The recursive walk owns an earlier frontier's diagnostic.
                if index + 2 != links.len() {
                    return;
                }
                self.lint_info_at(
                    Code::LintUntypedOptionalChain,
                    RULE,
                    "`?.` chain over an untyped value hedges every field".to_string(),
                    snode.span,
                    "declare a `type` for the value: decode untyped input once with \
                     `json_decode(text, schema_of(T))` or `schema_parse(value, schema_of(T))`, \
                     or annotate the parameter or return type that erased it to `dict`; then \
                     read typed fields with `.`"
                        .to_string(),
                );
                return;
            }
        }
    }

    /// `any`, `unknown`, a `dict` whose values are untyped, or a union of those
    /// with `nil`. A `dict<string, T>` with typed values is a real map, and
    /// `m?.key?.field` over it is ordinary nil handling.
    ///
    /// An open record (`{name: string, ...dict}` or `{name: string, ...R}`) is
    /// untyped for a key it does not declare when any tail is untyped, since
    /// that read can land in it, and typed for a key it declares.
    fn type_is_untyped_record(
        &self,
        ty: &TypeExpr,
        property: Option<&str>,
        scope: &TypeScope,
    ) -> bool {
        let ty = self.resolve_alias(ty, scope);
        match &ty {
            TypeExpr::Named(name) => name == "dict" || is_gradual_type_name(name),
            TypeExpr::Applied { name, args } if name == "dict" => args
                .last()
                .is_none_or(|value| self.map_value_is_untyped(value, scope)),
            TypeExpr::DictType(_, value) => self.map_value_is_untyped(value, scope),
            TypeExpr::OpenShape { fields, rests } => {
                let declared = property.is_some_and(|name| fields.iter().any(|f| f.name == name));
                !declared
                    && rests
                        .iter()
                        .any(|rest| self.open_row_tail_is_untyped(rest, scope))
            }
            TypeExpr::Union(members) => {
                let mut saw_untyped = false;
                for member in members {
                    if matches!(member, TypeExpr::Named(name) if name == "nil") {
                        continue;
                    }
                    if !self.type_is_untyped_record(member, property, scope) {
                        return false;
                    }
                    saw_untyped = true;
                }
                saw_untyped
            }
            _ => false,
        }
    }

    fn map_value_is_untyped(&self, value: &TypeExpr, scope: &TypeScope) -> bool {
        let value = self.resolve_alias(value, scope);
        match &value {
            TypeExpr::OpenShape { fields, .. } if !fields.is_empty() => false,
            TypeExpr::Union(members) => {
                let mut non_nil = members
                    .iter()
                    .filter(|member| !self.type_is_nil(member, scope));
                non_nil.next().is_some_and(|first| {
                    self.map_value_is_untyped(first, scope)
                        && non_nil.all(|member| self.map_value_is_untyped(member, scope))
                })
            }
            _ => self.type_is_untyped_record(&value, None, scope),
        }
    }

    /// A row tail that is an untyped dict or a still-generic row variable.
    fn open_row_tail_is_untyped(&self, rest: &TypeExpr, scope: &TypeScope) -> bool {
        matches!(rest, TypeExpr::Named(name) if scope.is_generic_type_param(name))
            || self.type_is_untyped_record(rest, None, scope)
    }
}
