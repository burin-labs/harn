//! Opt-in `require-file-header` rule: every file should begin with a
//! `/** */` doc block. Paired with a helper that derives the default
//! title from the filename when autofixing.

use harn_lexer::{FixEdit, Span};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

/// Emit `require-file-header` when the source does not begin with a
/// `/** */` doc block. Plain `//` line comments and non-doc `/*` blocks
/// both count as violations — only a `/**` block at the top satisfies it.
pub(crate) fn check_require_file_header(
    source: &str,
    file_path: Option<&std::path::Path>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    if i + 2 < bytes.len() && &bytes[i..i + 3] == b"/**" {
        return;
    }
    let title = derive_file_header_title(file_path);
    let header = format!("/**\n * {title}\n */\n\n");
    let span = Span::with_offsets(0, 0, 1, 1);
    diagnostics.push(LintDiagnostic {
        rule: "require-file-header",
        message: "file is missing a `/** */` header doc block".to_string(),
        span,
        severity: LintSeverity::Warning,
        suggestion: Some(format!(
            "add a `/** <title> */` block at the top of the file (e.g. `{title}`)"
        )),
        fix: Some(vec![FixEdit {
            span,
            replacement: header,
        }]),
    });
}

/// Derive the title shown inside the autofix's file-header block. Falls
/// back to a generic "Module." when no path is available. Only the first
/// letter is capitalized — not every word — per the header style.
pub fn derive_file_header_title(file_path: Option<&std::path::Path>) -> String {
    let stem = file_path
        .and_then(|p| p.file_stem().and_then(|s| s.to_str()))
        .unwrap_or("module");
    let mut cleaned = String::with_capacity(stem.len());
    for ch in stem.chars() {
        if ch == '-' || ch == '_' {
            cleaned.push(' ');
        } else {
            cleaned.push(ch);
        }
    }
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut trimmed = collapsed.trim().to_string();
    if trimmed.is_empty() {
        trimmed.push_str("module");
    }
    let mut chars = trimmed.chars();
    let head = chars.next().unwrap().to_ascii_uppercase();
    let tail: String = chars.collect();
    let mut out = String::new();
    out.push(head);
    out.push_str(&tail.to_lowercase());
    let last = out.chars().last().unwrap_or('.');
    if !matches!(last, '.' | '!' | '?') {
        out.push('.');
    }
    out
}
