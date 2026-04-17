//! Identifier-case helpers, item classifiers used by source-aware
//! rules, and small string utilities that don't belong with any single
//! rule.

use harn_parser::Node;

pub(crate) fn is_snake_case(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first == '_') {
        return false;
    }
    name.chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

pub(crate) fn is_pascal_case(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    name.chars().all(|ch| ch.is_ascii_alphanumeric())
}

pub(crate) fn to_snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

pub(crate) fn to_pascal_case(name: &str) -> String {
    let mut out = String::new();
    let mut uppercase_next = true;
    for ch in name.chars() {
        if !ch.is_ascii_alphanumeric() {
            uppercase_next = true;
            continue;
        }
        if uppercase_next {
            out.push(ch.to_ascii_uppercase());
            uppercase_next = false;
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

/// Top-level items for the `blank-line-between-items` rule. Includes
/// module-scope let/var bindings, which the plain "decl" set excludes.
pub(crate) fn is_top_level_item(node: &Node) -> bool {
    matches!(
        node,
        Node::FnDecl { .. }
            | Node::Pipeline { .. }
            | Node::StructDecl { .. }
            | Node::EnumDecl { .. }
            | Node::InterfaceDecl { .. }
            | Node::TypeDecl { .. }
            | Node::ToolDecl { .. }
            | Node::ImplBlock { .. }
            | Node::OverrideDecl { .. }
            | Node::LetBinding { .. }
            | Node::VarBinding { .. }
    )
}

pub(crate) fn is_import_item(node: &Node) -> bool {
    matches!(node, Node::ImportDecl { .. } | Node::SelectiveImport { .. })
}

/// Items whose preceding comments must use the canonical `/** */` form
/// for the `legacy-doc-comment` rule.
pub(crate) fn is_documentable_item(node: &Node) -> bool {
    matches!(
        node,
        Node::FnDecl { .. }
            | Node::Pipeline { .. }
            | Node::StructDecl { .. }
            | Node::EnumDecl { .. }
            | Node::InterfaceDecl { .. }
            | Node::TypeDecl { .. }
            | Node::ToolDecl { .. }
            | Node::ImplBlock { .. }
            | Node::OverrideDecl { .. }
    )
}

pub(crate) fn item_is_pub(node: &Node) -> bool {
    match node {
        Node::FnDecl { is_pub, .. }
        | Node::Pipeline { is_pub, .. }
        | Node::StructDecl { is_pub, .. }
        | Node::EnumDecl { is_pub, .. }
        | Node::ToolDecl { is_pub, .. } => *is_pub,
        // InterfaceDecl / ImplBlock / TypeDecl / OverrideDecl have no
        // is_pub flag — treat them as always-eligible when they appear at
        // the top level.
        Node::InterfaceDecl { .. }
        | Node::ImplBlock { .. }
        | Node::TypeDecl { .. }
        | Node::OverrideDecl { .. } => true,
        _ => false,
    }
}

/// Map 1-based line numbers to their starting byte offsets.
pub(crate) fn build_line_starts(source: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    starts.push(0);
    for (idx, ch) in source.char_indices() {
        if ch == '\n' {
            starts.push(idx + 1);
        }
    }
    starts
}

/// Simplify a boolean comparison expression like `x == true` → `x`.
pub fn simplify_bool_comparison(expr: &str) -> Option<String> {
    let trimmed = expr.trim();
    for op in &["==", "!="] {
        if let Some(idx) = trimmed.find(op) {
            let lhs = trimmed[..idx].trim();
            let rhs = trimmed[idx + op.len()..].trim();
            let (bool_val, other) = if rhs == "true" || rhs == "false" {
                (rhs, lhs)
            } else if lhs == "true" || lhs == "false" {
                (lhs, rhs)
            } else {
                continue;
            };
            let is_eq = *op == "==";
            let is_true = bool_val == "true";
            return if is_eq == is_true {
                Some(other.to_string())
            } else {
                Some(format!("!{other}"))
            };
        }
    }
    None
}
