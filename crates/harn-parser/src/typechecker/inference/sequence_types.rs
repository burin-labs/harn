//! Return types for sequence properties and methods.
//!
//! Empty sequences make element lookups nilable. Keeping that contract in one
//! place prevents method-call inference from disagreeing with property access
//! and with the VM's empty-sequence behavior.

use crate::ast::TypeExpr;

use super::super::scope::InferredType;
use super::super::union::simplify_union;
use super::super::TypeChecker;

impl TypeChecker {
    pub(in crate::typechecker) fn sequence_method_return_type(
        receiver: Option<&TypeExpr>,
        method: &str,
        arg_count: usize,
        list_element: Option<&TypeExpr>,
    ) -> InferredType {
        let list_receiver = list_element.is_some()
            || matches!(receiver, Some(TypeExpr::Named(name)) if name == "list");

        match method {
            "first" | "last" if list_receiver && arg_count == 0 => Some(simplify_union(vec![
                list_element
                    .cloned()
                    .unwrap_or_else(|| TypeExpr::Named("any".into())),
                TypeExpr::Named("nil".into()),
            ])),
            "first" | "last" if list_receiver => Some(match list_element {
                Some(element) => TypeExpr::List(Box::new(element.clone())),
                None => TypeExpr::Named("list".into()),
            }),
            "find" if list_receiver => Some(simplify_union(vec![
                list_element
                    .cloned()
                    .unwrap_or_else(|| TypeExpr::Named("any".into())),
                TypeExpr::Named("nil".into()),
            ])),
            "first" | "last" if matches!(receiver, Some(TypeExpr::Named(name)) if name == "range") => {
                Some(simplify_union(vec![
                    TypeExpr::Named("int".into()),
                    TypeExpr::Named("nil".into()),
                ]))
            }
            _ => None,
        }
    }

    pub(in crate::typechecker) fn list_property_type(
        item_type: Option<&TypeExpr>,
        property: &str,
        optional_access: bool,
    ) -> InferredType {
        match property {
            "count" => Some(TypeExpr::Named("int".into())),
            "empty" => Some(TypeExpr::Named("bool".into())),
            "first" | "last" => item_type
                .map(|inner| simplify_union(vec![inner.clone(), TypeExpr::Named("nil".into())])),
            _ if optional_access => Some(TypeExpr::Named("nil".into())),
            _ => None,
        }
    }
}
