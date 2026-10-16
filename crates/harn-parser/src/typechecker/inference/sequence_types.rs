//! Return types for sequence properties.
//!
//! Empty sequences make element lookups nilable, matching the `T?` results
//! the method registry declares for `first()` / `last()`.

use crate::ast::TypeExpr;

use super::super::scope::InferredType;
use super::super::union::simplify_union;
use super::super::TypeChecker;

impl TypeChecker {
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
