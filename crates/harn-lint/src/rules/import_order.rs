//! `import-order` rule: imports must appear in canonical order — stdlib
//! first, then alphabetical by path, with selective imports sorted
//! after bare imports for the same path.

use harn_lexer::{FixEdit, Span};
use harn_parser::{Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::naming::is_import_item;

/// Emit `import-order` diagnostics when imports are out of canonical
/// order (stdlib first, alphabetical by path, selective imports after
/// bare imports for the same path).
pub(crate) fn check_import_order(
    source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let mut imports: Vec<&SNode> = Vec::new();
    for node in program {
        if is_import_item(&node.node) {
            imports.push(node);
        } else {
            break;
        }
    }
    if imports.len() < 2 {
        return;
    }
    let mut sorted = imports.clone();
    sorted.sort_by_key(|a| import_sort_key(a));
    let already_sorted = imports
        .iter()
        .zip(sorted.iter())
        .all(|(a, b)| std::ptr::eq(*a, *b));
    if already_sorted {
        return;
    }

    // Autofix just emits each import slice joined by newlines; the
    // formatter re-normalizes spacing in a later pass.
    let first = imports.first().unwrap();
    let last = imports.last().unwrap();
    let replacement = sorted
        .iter()
        .map(|n| render_import_source(source, n))
        .collect::<Vec<_>>()
        .join("\n");
    let replace_span = Span::with_offsets(
        first.span.start,
        last.span.end,
        first.span.line,
        first.span.column,
    );
    diagnostics.push(LintDiagnostic {
        rule: "import-order",
        message: "imports are not in canonical order (stdlib first, then alphabetical by path)"
            .to_string(),
        span: replace_span,
        severity: LintSeverity::Warning,
        suggestion: Some(
            "reorder imports: std/ first, then third-party and local paths alphabetically"
                .to_string(),
        ),
        fix: Some(vec![FixEdit {
            span: replace_span,
            replacement,
        }]),
    });
}

fn import_sort_key(node: &SNode) -> (u8, String, u8, String) {
    match &node.node {
        Node::ImportDecl { path, .. } => (
            u8::from(!path.starts_with("std/")),
            path.clone(),
            0,
            String::new(),
        ),
        Node::SelectiveImport { names, path, .. } => {
            let mut sorted_names = names.clone();
            sorted_names.sort();
            (
                u8::from(!path.starts_with("std/")),
                path.clone(),
                1,
                sorted_names.join(","),
            )
        }
        _ => (2, String::new(), 2, String::new()),
    }
}

/// Slice the raw source covered by an import node's span.
fn render_import_source(source: &str, node: &SNode) -> String {
    source
        .get(node.span.start..node.span.end)
        .unwrap_or("")
        .to_string()
}
