//! Reference-path narrowing for inferred property and subscript types.

use super::*;
use crate::typechecker::{is_gradual_type_name, scope::InferredType, union::subtract_type};

impl TypeChecker {
    /// Re-apply a [`PathNarrowing`] directive to a reference path's natural
    /// type. The directives reuse the same type algebra that variable
    /// narrowing uses (`narrow_to_single`/`remove_from_union` for `type_of`,
    /// `intersect_types`/`subtract_type` for schema checks), treating a
    /// non-union type as a one-member union. A top type (`unknown`/`any`)
    /// narrows straight to the tested kind on `Keep`, mirroring variable
    /// `unknown` narrowing. When a directive matches nothing (a statically
    /// dead branch) the natural type is returned unchanged rather than
    /// over-narrowing.
    pub(in crate::typechecker::inference) fn apply_path_narrowing(
        natural: InferredType,
        narrowing: &PathNarrowing,
    ) -> InferredType {
        let ty = natural?;
        match narrowing {
            PathNarrowing::Keep(tag) => {
                if matches!(&ty, TypeExpr::Named(n) if is_gradual_type_name(n)) {
                    return Some(TypeExpr::Named(tag.clone()));
                }
                let members = Self::as_union_members(&ty);
                narrow_to_single(&members, tag).or(Some(ty))
            }
            PathNarrowing::Remove(tag) => {
                let members = Self::as_union_members(&ty);
                remove_from_union(&members, tag).or(Some(ty))
            }
            PathNarrowing::Intersect(schema) => intersect_types(&ty, schema).or(Some(ty)),
            PathNarrowing::Subtract(schema) => subtract_type(&ty, schema).or(Some(ty)),
            // Invalidated by a base reassignment: natural type unchanged.
            PathNarrowing::Cleared => Some(ty),
        }
    }

    fn as_union_members(ty: &TypeExpr) -> Vec<TypeExpr> {
        match ty {
            TypeExpr::Union(members) => members.clone(),
            other => vec![other.clone()],
        }
    }
}
