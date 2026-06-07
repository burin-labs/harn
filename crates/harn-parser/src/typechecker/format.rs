//! Display helpers for type expressions and shape mismatches.
//!
//! `format_type` is the canonical pretty-printer for `TypeExpr` (also used
//! by `harn-lsp` and `harn-fmt` via re-export). `shape_mismatch_detail`
//! produces a one-line "missing field …" / "field 'x' has type …" diff that
//! enriches type-error messages.

use crate::ast::*;

/// Pretty-print a type expression for display in error messages.
pub fn format_type(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(n) => n.clone(),
        TypeExpr::Union(types) => {
            if let Some(inner) = optional_sugar_inner(types) {
                return format!("{}?", format_type(inner));
            }
            types
                .iter()
                .map(format_type)
                .collect::<Vec<_>>()
                .join(" | ")
        }
        TypeExpr::Intersection(types) => types
            .iter()
            .map(|m| match m {
                // `T | nil` arms render as the sugared `T?`, which binds
                // tighter than `&` and reads back unambiguously.
                TypeExpr::Union(members) if optional_sugar_inner(members).is_some() => {
                    format_type(m)
                }
                // Other nested unions still get parenthesised for readability.
                TypeExpr::Union(_) => format!("({})", format_type(m)),
                _ => format_type(m),
            })
            .collect::<Vec<_>>()
            .join(" & "),
        TypeExpr::Shape(fields) => {
            let inner: Vec<String> = fields
                .iter()
                .map(|f| {
                    let opt = if f.optional { "?" } else { "" };
                    format!("{}{opt}: {}", f.name, format_type(&f.type_expr))
                })
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        TypeExpr::OpenShape { fields, rests } => {
            let mut parts: Vec<String> = fields
                .iter()
                .map(|f| {
                    let opt = if f.optional { "?" } else { "" };
                    format!("{}{opt}: {}", f.name, format_type(&f.type_expr))
                })
                .collect();
            for rest in rests {
                parts.push(format!("...{}", format_type(rest)));
            }
            format!("{{{}}}", parts.join(", "))
        }
        TypeExpr::List(inner) => format!("list<{}>", format_type(inner)),
        TypeExpr::Iter(inner) => format!("iter<{}>", format_type(inner)),
        TypeExpr::Generator(inner) => format!("Generator<{}>", format_type(inner)),
        TypeExpr::Stream(inner) => format!("Stream<{}>", format_type(inner)),
        TypeExpr::DictType(k, v) => format!("dict<{}, {}>", format_type(k), format_type(v)),
        TypeExpr::Applied { name, args } => {
            let args_str = args.iter().map(format_type).collect::<Vec<_>>().join(", ");
            format!("{name}<{args_str}>")
        }
        TypeExpr::FnType {
            params,
            return_type,
        } => {
            let params_str = params
                .iter()
                .map(format_type)
                .collect::<Vec<_>>()
                .join(", ");
            format!("fn({}) -> {}", params_str, format_type(return_type))
        }
        TypeExpr::Never => "never".to_string(),
        TypeExpr::LitString(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        TypeExpr::LitInt(v) => v.to_string(),
        TypeExpr::Owned(inner) => format!("owned<{}>", format_type(inner)),
    }
}

/// Produce a detail string describing why a Shape type is incompatible with
/// another Shape type — e.g. "missing field 'age' (int)" or "field 'name'
/// has type int, expected string". Returns `None` if both types are not shapes.
pub fn shape_mismatch_detail(expected: &TypeExpr, actual: &TypeExpr) -> Option<String> {
    if let (TypeExpr::Shape(ef), TypeExpr::Shape(af)) = (expected, actual) {
        let mut details = Vec::new();
        for field in ef {
            if field.optional {
                continue;
            }
            match af.iter().find(|f| f.name == field.name) {
                None => details.push(format!(
                    "missing field '{}' ({})",
                    field.name,
                    format_type(&field.type_expr)
                )),
                Some(actual_field) => {
                    let e_str = format_type(&field.type_expr);
                    let a_str = format_type(&actual_field.type_expr);
                    if e_str != a_str {
                        details.push(format!(
                            "field '{}' has type {}, expected {}",
                            field.name, a_str, e_str
                        ));
                    }
                }
            }
        }
        if details.is_empty() {
            None
        } else {
            Some(details.join("; "))
        }
    } else {
        None
    }
}

/// If `types` is exactly two members and one is `nil`, return the
/// non-`nil` member when it can be safely rendered as `T?`. Mirrors the
/// formatter's rule in `harn-fmt::helpers::optional_sugar_inner`: only
/// types that appear at primary precedence (or below) can be sugared,
/// because postfix `?` parses tighter than `&` / `|` / `fn(...) -> ...`
/// return positions.
fn optional_sugar_inner(types: &[TypeExpr]) -> Option<&TypeExpr> {
    if types.len() != 2 {
        return None;
    }
    let nil_idx = types
        .iter()
        .position(|t| matches!(t, TypeExpr::Named(n) if n == "nil"))?;
    let inner = &types[1 - nil_idx];
    if matches!(
        inner,
        TypeExpr::Union(_) | TypeExpr::Intersection(_) | TypeExpr::FnType { .. }
    ) {
        return None;
    }
    if matches!(inner, TypeExpr::Named(n) if n == "nil") {
        return None;
    }
    Some(inner)
}

/// Returns true when the type is obvious from the RHS expression
/// (e.g. `let x = 42` is obviously int — no hint needed).
pub(super) fn is_obvious_type(value: &SNode, _ty: &TypeExpr) -> bool {
    matches!(
        &value.node,
        Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::StringLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::ListLiteral(_)
            | Node::DictLiteral(_)
            | Node::InterpolatedString(_)
    )
}
