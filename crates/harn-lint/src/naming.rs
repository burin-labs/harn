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

// Rename suggestions reuse the same case conversion the `strings` builtins
// expose, so a fixit the linter proposes is exactly what `camel_to_snake`
// would produce at runtime.
pub(crate) use harn_vm::text::case::{to_pascal_case, to_snake_case};

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
            | Node::SkillDecl { .. }
            | Node::EvalPackDecl { .. }
            | Node::ImplBlock { .. }
            | Node::OverrideDecl { .. }
            | Node::LetBinding { .. }
            | Node::ConstBinding { .. }
    )
}

pub(crate) fn is_import_item(node: &Node) -> bool {
    matches!(
        node,
        Node::ImportDecl { .. } | Node::NamespaceImport { .. } | Node::SelectiveImport { .. }
    )
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
            | Node::SkillDecl { .. }
            | Node::EvalPackDecl { .. }
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
        | Node::ToolDecl { is_pub, .. }
        | Node::SkillDecl { is_pub, .. }
        | Node::EvalPackDecl { is_pub, .. } => *is_pub,
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
