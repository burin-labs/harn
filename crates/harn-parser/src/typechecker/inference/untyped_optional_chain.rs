//! `HARN-LNT-080`: a `?.` chain over an untyped value.

use crate::ast::*;
use crate::diagnostic_codes::Code;

use super::super::scope::TypeScope;
use super::super::{is_gradual_type_name, TypeChecker};

const RULE: &str = "untyped-optional-chain";

impl TypeChecker {
    /// Warn on the second `?.` link of a chain whose first optional receiver
    /// is untyped (`any`, `unknown`, an open `dict`, or nothing inferred).
    ///
    /// `data?.repository?.pullRequest` hedges every field because nothing
    /// declared the shape. Decoding once with `schema_parse(value,
    /// schema_of(T))` validates it and leaves typed fields behind. A chain
    /// over a typed record with optional fields is real nil handling and is
    /// left alone. Reporting only the second link gives one warning per chain.
    pub(in crate::typechecker) fn check_untyped_optional_chain(
        &mut self,
        snode: &SNode,
        object: &SNode,
        scope: &TypeScope,
    ) {
        let mut links = 0;
        let mut innermost_receiver = None;
        let mut innermost_property = None;
        let mut cursor = object;
        loop {
            match &cursor.node {
                Node::OptionalPropertyAccess { object, property } => {
                    links += 1;
                    innermost_receiver = Some(object.as_ref());
                    innermost_property = Some(property.as_str());
                    cursor = object;
                }
                Node::OptionalSubscriptAccess { object, .. } => {
                    links += 1;
                    innermost_receiver = Some(object.as_ref());
                    innermost_property = None;
                    cursor = object;
                }
                Node::PropertyAccess { object, .. } | Node::SubscriptAccess { object, .. } => {
                    cursor = object;
                }
                _ => break,
            }
        }
        if links != 1 {
            return;
        }
        let Some(receiver) = innermost_receiver else {
            return;
        };
        let receiver_type = self.infer_type(receiver, scope);
        if receiver_type
            .as_ref()
            .is_some_and(|ty| !self.type_is_untyped_record(ty, innermost_property, scope))
        {
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
    }

    /// `any`, `unknown`, a `dict` whose values are untyped, or a union of those
    /// with `nil`. A `dict<string, T>` with typed values is a real map, and
    /// `m?.key?.field` over it is ordinary nil handling.
    ///
    /// An open record (`{name: string, ...dict}` or `{name: string, ...R}`) is untyped for a key it does
    /// not declare, since that read lands in the untyped tail, and typed for
    /// one it does.
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
                .is_none_or(|value| self.type_is_untyped_record(value, None, scope)),
            TypeExpr::DictType(_, value) => self.type_is_untyped_record(value, None, scope),
            TypeExpr::OpenShape { fields, rests } => {
                let declared = property.is_some_and(|name| fields.iter().any(|f| f.name == name));
                !declared
                    && rests
                        .iter()
                        .all(|rest| self.open_row_tail_is_untyped(rest, scope))
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

    /// A row tail that is an untyped dict or a still-generic row variable.
    fn open_row_tail_is_untyped(&self, rest: &TypeExpr, scope: &TypeScope) -> bool {
        matches!(rest, TypeExpr::Named(name) if scope.is_generic_type_param(name))
            || self.type_is_untyped_record(rest, None, scope)
    }
}
