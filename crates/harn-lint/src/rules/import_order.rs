//! `import-order` rule: imports must appear in canonical order — stdlib
//! first, then alphabetical by path, with selective imports sorted
//! after bare imports for the same path.

use harn_lexer::{FixEdit, Span};
use harn_parser::{DiagnosticCode as Code, Node, SNode};

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

    let first = imports.first().unwrap();
    let last = imports.last().unwrap();
    let replace_start = import_source_start(source, first).unwrap_or(first.span.start);
    let replace_span = Span::with_offsets(
        replace_start,
        last.span.end,
        first.span.line,
        first.span.column,
    );
    let fix = if import_block_has_comments(source, first.span.start, last.span.end) {
        None
    } else {
        let replacement = sorted
            .iter()
            .map(|n| render_import_source(source, n))
            .collect::<Option<Vec<_>>>()
            .map(|imports| imports.join("\n"));
        replacement.map(|replacement| {
            vec![FixEdit {
                span: replace_span,
                replacement,
            }]
        })
    };
    diagnostics.push(LintDiagnostic {
        code: Code::LintImportOrder,
        rule: "import-order".into(),
        message: "imports are not in canonical order (stdlib first, then alphabetical by path)"
            .to_string(),
        span: replace_span,
        severity: LintSeverity::Warning,
        suggestion: Some(
            "reorder imports: std/ first, then third-party and local paths alphabetically"
                .to_string(),
        ),
        fix,
    });
}

fn import_sort_key(node: &SNode) -> (u8, String, u8, String) {
    match &node.node {
        // Kind order: wildcard=0, namespace=1, selective=2.
        Node::ImportDecl { path, .. } => (
            u8::from(!path.starts_with("std/")),
            path.clone(),
            0,
            String::new(),
        ),
        Node::NamespaceImport { alias, path, .. } => (
            u8::from(!path.starts_with("std/")),
            path.clone(),
            1,
            alias.clone(),
        ),
        Node::SelectiveImport { names, path, .. } => {
            let mut sorted_names = names.clone();
            sorted_names.sort();
            (
                u8::from(!path.starts_with("std/")),
                path.clone(),
                2,
                sorted_names.join(","),
            )
        }
        _ => (3, String::new(), 3, String::new()),
    }
}

/// Slice the complete import declaration. Parser spans begin at `import`, so a
/// public declaration must deliberately reclaim its preceding `pub` token.
/// Refuse the fix when that ownership cannot be proven instead of silently
/// changing the module's exported surface.
fn render_import_source(source: &str, node: &SNode) -> Option<String> {
    let start = import_source_start(source, node)?;
    Some(source.get(start..node.span.end)?.to_string())
}

fn import_source_start(source: &str, node: &SNode) -> Option<usize> {
    if !import_is_public(&node.node) {
        return Some(node.span.start);
    }
    let source_before_import = source.get(..node.span.start)?;
    let line_start = source_before_import
        .rfind('\n')
        .map_or(0, |newline| newline + 1);
    let prefix = source.get(line_start..node.span.start)?;
    let pub_offset = prefix.find("pub")?;
    let before_pub = prefix.get(..pub_offset)?;
    let after_pub = prefix.get(pub_offset + "pub".len()..)?;
    (before_pub.trim().is_empty() && after_pub.trim().is_empty()).then_some(line_start + pub_offset)
}

fn import_is_public(node: &Node) -> bool {
    match node {
        Node::ImportDecl { is_pub, .. }
        | Node::NamespaceImport { is_pub, .. }
        | Node::SelectiveImport { is_pub, .. } => *is_pub,
        _ => false,
    }
}

fn import_block_has_comments(source: &str, start: usize, end: usize) -> bool {
    source.get(start..end).is_some_and(|block| {
        block
            .lines()
            .any(|line| line.trim_start().starts_with("//") || line.contains("/*"))
    })
}
