//! Type rules for binary operators.
//!
//! Pure helper that maps `(operator, left_type, right_type)` to the inferred
//! result type without touching scope or emitting diagnostics. The
//! diagnostic-emitting binary-op checker lives on `TypeChecker` itself
//! (`check_binops`) — see `inference::binary_ops`.

use crate::ast::*;

use super::scope::InferredType;

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
            // Union containing nil: strip nil, use non-nil members
            (Some(TypeExpr::Union(members)), _) => {
                let non_nil: Vec<_> = members
                    .iter()
                    .filter(|m| !matches!(m, TypeExpr::Named(n) if n == "nil"))
                    .cloned()
                    .collect();
                if non_nil.len() == 1 {
                    Some(non_nil[0].clone())
                } else if non_nil.is_empty() {
                    right.clone()
                } else {
                    Some(TypeExpr::Union(non_nil))
                }
            }
            // Left is nil: result is always the right side
            (Some(TypeExpr::Named(n)), _) if n == "nil" => right.clone(),
            // Left is a known non-nil type: right is unreachable, preserve left
            (Some(l), _) => Some(l.clone()),
            // Unknown left: use right as best guess
            (None, _) => right.clone(),
        },
        "|>" => None,
        _ => None,
    }
}
