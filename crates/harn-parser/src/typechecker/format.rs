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
        TypeExpr::Union(types) => types
            .iter()
            .map(format_type)
            .collect::<Vec<_>>()
            .join(" | "),
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
