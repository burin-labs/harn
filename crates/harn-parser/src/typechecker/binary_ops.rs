//! Type rules for binary operators.
//!
//! Pure helper that maps `(operator, left_type, right_type)` to the inferred
//! result type without touching scope or emitting diagnostics. The
//! diagnostic-emitting binary-op checker lives on `TypeChecker` itself
//! (`check_binops`) — see `inference::binary_ops`.

use crate::ast::*;

use super::scope::InferredType;
use super::union::{contains_nil, simplify_union, without_nil};

fn dict_like(ty: &TypeExpr) -> bool {
    matches!(ty, TypeExpr::Named(n) if n == "dict")
        || matches!(ty, TypeExpr::DictType(..))
        || matches!(ty, TypeExpr::Shape(_))
}

/// Right-biased structural merge of two record shapes — the type-level model
/// of `merge(a, b)` / `{...a, ...b}` / `a + b`, where `b`'s fields override
/// `a`'s on overlap (matching the runtime). Field rules (see the row-poly
/// design memo §2.4):
///
/// - A field present on only one side carries through unchanged.
/// - On overlap, the result is **present (required) if either side is
///   required** (`optional = a.optional && b.optional`).
/// - The overlap value type is `b`'s type when `b`'s field is required (`b`
///   always overrides), otherwise `a | b` (when `b` is absent at runtime the
///   value comes from `a`).
///
/// This is the precise, sound result for *closed* shapes: `merge({a:int,
/// b:int}, {b:str, c:bool})` → `{a:int, b:str, c:bool}` — every field
/// preserved (the carry-forward that plain subtyping/intersection can't give).
pub(in crate::typechecker) fn merge_shape_fields(
    left: &[ShapeField],
    right: &[ShapeField],
) -> Vec<ShapeField> {
    let mut out: Vec<ShapeField> = left.to_vec();
    for rf in right {
        if let Some(slot) = out.iter_mut().find(|f| f.name == rf.name) {
            slot.optional = slot.optional && rf.optional;
            slot.type_expr = if rf.optional {
                simplify_union(vec![slot.type_expr.clone(), rf.type_expr.clone()])
            } else {
                rf.type_expr.clone()
            };
        } else {
            out.push(rf.clone());
        }
    }
    out
}

/// Infer the result type of a binary operation.
pub(super) fn infer_binary_op_type(
    op: &str,
    left: &InferredType,
    right: &InferredType,
) -> InferredType {
    match op {
        "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" | "in" | "not_in" => {
            Some(TypeExpr::Named("bool".into()))
        }
        "+" => match (left, right) {
            (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) => {
                match (l.as_str(), r.as_str()) {
                    ("int", "int") => Some(TypeExpr::Named("int".into())),
                    ("float", _) | (_, "float") => Some(TypeExpr::Named("float".into())),
                    ("string", "string") => Some(TypeExpr::Named("string".into())),
                    ("list", "list") => Some(TypeExpr::Named("list".into())),
                    ("dict", "dict") => Some(TypeExpr::Named("dict".into())),
                    _ => None,
                }
            }
            // Two closed record shapes merge to the precise right-biased
            // merged shape (`b` overrides `a`), instead of collapsing to a
            // bare `dict` and losing every field type.
            (Some(TypeExpr::Shape(l)), Some(TypeExpr::Shape(r))) => {
                Some(TypeExpr::Shape(merge_shape_fields(l, r)))
            }
            // dict + dict shallow-merges. Recognize parameterized and shape
            // forms so a `dict<string, V> + Shape{...}` chain (the common
            // connector/agent option-projection idiom) keeps the dict-shaped
            // typing instead of falling through to `None` and downgrading
            // every subsequent inference step to untyped. (A bare/parameterised
            // dict on either side means an unknown tail, so the merged shape is
            // not statically closed — degrade to `dict`.)
            (Some(l), Some(r)) if dict_like(l) && dict_like(r) => {
                Some(TypeExpr::Named("dict".into()))
            }
            _ => None,
        },
        "-" | "/" | "%" => match (left, right) {
            (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) => {
                match (l.as_str(), r.as_str()) {
                    ("int", "int") => Some(TypeExpr::Named("int".into())),
                    ("float", _) | (_, "float") => Some(TypeExpr::Named("float".into())),
                    _ => None,
                }
            }
            _ => None,
        },
        "**" => match (left, right) {
            (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) => {
                match (l.as_str(), r.as_str()) {
                    ("int", "int") => Some(TypeExpr::Named("int".into())),
                    ("float", _) | (_, "float") => Some(TypeExpr::Named("float".into())),
                    _ => None,
                }
            }
            _ => None,
        },
        "*" => match (left, right) {
            (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) => {
                match (l.as_str(), r.as_str()) {
                    ("string", "int") | ("int", "string") => Some(TypeExpr::Named("string".into())),
                    ("int", "int") => Some(TypeExpr::Named("int".into())),
                    ("float", _) | (_, "float") => Some(TypeExpr::Named("float".into())),
                    _ => None,
                }
            }
            _ => None,
        },
        "??" => match (left, right) {
            (Some(left), right) => match without_nil(left) {
                // Left is only nil, so the result is exactly the fallback.
                None => right.clone(),
                // Left cannot be nil, so the fallback is unreachable.
                Some(non_nil_left) if !contains_nil(left) => Some(non_nil_left),
                // Left may be nil; include the fallback when its type is known.
                Some(non_nil_left) => match right {
                    Some(right) if &non_nil_left == right => Some(non_nil_left),
                    Some(right) => Some(simplify_union(vec![non_nil_left, right.clone()])),
                    None => Some(non_nil_left),
                },
            },
            // Unknown left: use right as best guess.
            (None, _) => right.clone(),
        },
        "|>" => None,
        _ => None,
    }
}
