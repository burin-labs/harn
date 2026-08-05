//! Structural validation for fresh record literals at call boundaries.

use std::collections::BTreeMap;

use crate::ast::{Node, SNode, TypeExpr};
use crate::diagnostic_codes::Code;

use super::super::scope::TypeScope;
use super::super::TypeChecker;

impl TypeChecker {
    /// Reject unsupported keys when a fresh record literal is passed to a
    /// closed-record contract. Previously inferred values keep ordinary width
    /// subtyping, so internal records can carry more data without weakening
    /// literal validation at the API boundary.
    pub(in crate::typechecker) fn check_unknown_closed_record_fields(
        &mut self,
        context: impl Into<String>,
        expected: &TypeExpr,
        arg: &SNode,
        scope: &TypeScope,
    ) {
        let Node::DictLiteral(entries) = &arg.node else {
            return;
        };
        fn collect_closed_fields(ty: &TypeExpr, fields: &mut BTreeMap<String, TypeExpr>) -> bool {
            match ty {
                TypeExpr::Shape(shape_fields) => {
                    for field in shape_fields {
                        fields
                            .entry(field.name.clone())
                            .and_modify(|existing| {
                                *existing = TypeExpr::Intersection(vec![
                                    existing.clone(),
                                    field.type_expr.clone(),
                                ]);
                            })
                            .or_insert_with(|| field.type_expr.clone());
                    }
                    true
                }
                TypeExpr::Intersection(members) => members
                    .iter()
                    .all(|member| collect_closed_fields(member, fields)),
                _ => false,
            }
        }

        let resolved = self.resolve_alias(expected, scope);
        let mut known = BTreeMap::new();
        let is_closed = match &resolved {
            TypeExpr::Union(members) => {
                let non_nil = members
                    .iter()
                    .filter(|member| !matches!(member, TypeExpr::Named(name) if name == "nil"))
                    .collect::<Vec<_>>();
                non_nil.len() == 1 && collect_closed_fields(non_nil[0], &mut known)
            }
            other => collect_closed_fields(other, &mut known),
        };
        if !is_closed {
            return;
        }
        let expected_list = known
            .keys()
            .map(|field| format!("`{field}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let context = context.into();
        for entry in entries {
            if matches!(entry.value.node, Node::Spread(_)) {
                continue;
            }
            let key = match &entry.key.node {
                Node::StringLiteral(key) | Node::Identifier(key) => key,
                _ => continue,
            };
            if let Some(field_type) = known.get(key) {
                self.check_unknown_closed_record_fields(
                    format!("{context}.{key}"),
                    field_type,
                    &entry.value,
                    scope,
                );
                continue;
            }
            let mut message = format!("{context}: unknown field `{key}` in closed record");
            if !expected_list.is_empty() {
                message.push_str(&format!("; expected one of {expected_list}"));
            }
            if let Some(candidate) =
                crate::diagnostic::find_closest_match(key, known.keys().map(String::as_str), 3)
            {
                message.push_str(&format!(" — did you mean `{candidate}`?"));
            }
            self.error_at(Code::ArgumentTypeMismatch, message, entry.key.span);
        }
    }
}
