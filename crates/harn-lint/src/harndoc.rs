//! HarnDoc comment scanning, canonicalization helpers, and the
//! `legacy-doc-comment` rule. Keeps lexer re-entry and replacement-text
//! construction isolated from the linter walk.

use harn_lexer::{FixEdit, Span};
use harn_parser::{DiagnosticCode as Code, Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::naming::{is_documentable_item, item_is_pub};

/// A comment token recovered from a re-lex of the source.
#[derive(Clone)]
pub(crate) struct LegacyCommentTok {
    /// 1-based line where the comment *ends* (the lexer's `span.line`).
    pub(crate) line: usize,
    /// 1-based line where the comment *begins*. Equal to `line` for line
    /// comments; smaller for a multi-line block comment.
    pub(crate) start_line: usize,
    pub(crate) start_byte: usize,
    pub(crate) end_byte: usize,
    pub(crate) is_line: bool,
    pub(crate) is_doc: bool,
    pub(crate) text: String,
}

/// Walk the source with the lexer and return a vector of line-comment and
/// block-comment tokens, in source order.
pub(crate) fn collect_comment_tokens(source: &str) -> Vec<LegacyCommentTok> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let Ok(tokens) = lexer.tokenize_with_comments() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for tok in tokens {
        match tok.kind {
            harn_lexer::TokenKind::LineComment { text, is_doc } => {
                out.push(LegacyCommentTok {
                    line: tok.span.line,
                    start_line: tok.span.line,
                    start_byte: tok.span.start,
                    end_byte: tok.span.end,
                    is_line: true,
                    is_doc,
                    text,
                });
            }
            harn_lexer::TokenKind::BlockComment { text, is_doc } => {
                // The lexer keeps internal newlines in `text` and stamps the
                // span line at the comment's *end*, so the start line is the
                // end line minus the number of contained newlines.
                let newlines = text.matches('\n').count();
                out.push(LegacyCommentTok {
                    line: tok.span.line,
                    start_line: tok.span.line.saturating_sub(newlines),
                    start_byte: tok.span.start,
                    end_byte: tok.span.end,
                    is_line: false,
                    is_doc,
                    text,
                });
            }
            _ => {}
        }
    }
    out
}

/// Collect the contiguous run of *migratable* comment tokens directly above
/// `item_line` (1-based), returned in source order (top to bottom).
///
/// A run is the set of comments with no blank-line gap between them and the
/// item. The walk stops at a blank line, a gap, or a canonical `/** */` doc
/// block (already correct, never rewritten). The returned tokens are exactly
/// the ones `legacy-doc-comment` will rewrite: `//` / `///` lines and plain
/// `/* */` block comments.
fn migratable_run_above<'c>(
    by_line: &std::collections::HashMap<usize, &'c LegacyCommentTok>,
    item_line: usize,
) -> Vec<&'c LegacyCommentTok> {
    let mut walked: Vec<&LegacyCommentTok> = Vec::new();
    let mut cursor = item_line.saturating_sub(1);
    while cursor > 0 {
        let Some(tok) = by_line.get(&cursor) else {
            break;
        };
        // A canonical `/** */` doc block is already correct; it stops the
        // walk and is never collected for rewriting.
        if !tok.is_line && tok.is_doc {
            break;
        }
        walked.push(*tok);
        // Step past the whole token (multi-line blocks span several lines).
        cursor = tok.start_line.saturating_sub(1);
    }
    walked.reverse();
    walked
}

/// Returns true when there is a contiguous run of wrong-format comments
/// (`//` / `///` lines or a plain `/* */` block) directly above the item on
/// `item_line` that `legacy-doc-comment` will migrate to `/** */`. Used to
/// suppress the redundant fixless `missing-harndoc` diagnostic in that case
/// — the migration already covers it with an actual autofix.
pub(crate) fn run_above_is_migratable(comments: &[LegacyCommentTok], item_line: usize) -> bool {
    if item_line == 0 || comments.is_empty() {
        return false;
    }
    let by_line: std::collections::HashMap<usize, &LegacyCommentTok> =
        comments.iter().map(|c| (c.line, c)).collect();
    !migratable_run_above(&by_line, item_line).is_empty()
}

/// Produce the canonical `/** */` replacement text for a run of comment
/// tokens. `body_lines` contains one text line per collected comment (already
/// stripped of `//` / `///` markers). The return value does NOT include a
/// trailing newline — the replacement span covers exactly the original
/// comment lines' textual range.
pub(crate) fn canonical_doc_block(
    body_lines: &[String],
    indent: usize,
    line_width: usize,
) -> String {
    let indent_str = " ".repeat(indent);
    let mut start = 0;
    while start < body_lines.len() && body_lines[start].trim().is_empty() {
        start += 1;
    }
    let mut end = body_lines.len();
    while end > start && body_lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    let body = &body_lines[start..end];
    if body.is_empty() {
        return format!("{indent_str}/** */");
    }
    if body.len() == 1 {
        let only = body[0].trim();
        let compact = format!("{indent_str}/** {only} */");
        if compact.len() <= line_width {
            return compact;
        }
    }
    let mut out = String::new();
    out.push_str(&indent_str);
    out.push_str("/**");
    for line in body {
        out.push('\n');
        if line.trim().is_empty() {
            out.push_str(&indent_str);
            out.push_str(" *");
        } else {
            out.push_str(&indent_str);
            out.push_str(" * ");
            out.push_str(line.trim_end());
        }
    }
    out.push('\n');
    out.push_str(&indent_str);
    out.push_str(" */");
    out
}

/// Collect and emit `legacy-doc-comment` diagnostics. Walks top-level items
/// plus `pub` methods inside `impl` blocks, looks for a contiguous run of
/// `///` lines (or `//` lines with no blank line between the run and the
/// item), and produces an autofix replacement with the canonical form.
pub(crate) fn check_legacy_doc_comments(
    source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let comments = collect_comment_tokens(source);
    if comments.is_empty() {
        return;
    }
    let by_line: std::collections::HashMap<usize, &LegacyCommentTok> =
        comments.iter().map(|c| (c.line, c)).collect();

    fn visit(
        node: &SNode,
        comments: &[LegacyCommentTok],
        by_line: &std::collections::HashMap<usize, &LegacyCommentTok>,
        source: &str,
        diagnostics: &mut Vec<LintDiagnostic>,
        is_top_level: bool,
    ) {
        // Eligible only when top-level or explicitly `pub`; impl methods
        // are documented relative to their impl block.
        if is_documentable_item(&node.node) && (is_top_level || item_is_pub(&node.node)) {
            check_one_item(node, comments, by_line, source, diagnostics);
        }
        match &node.node {
            Node::Pipeline { body, .. }
            | Node::FnDecl { body, .. }
            | Node::ToolDecl { body, .. }
            | Node::OverrideDecl { body, .. } => {
                for child in body {
                    visit(child, comments, by_line, source, diagnostics, false);
                }
            }
            Node::SkillDecl { fields, .. } => {
                for (_k, v) in fields {
                    visit(v, comments, by_line, source, diagnostics, false);
                }
            }
            Node::EvalPackDecl {
                fields,
                body,
                summarize,
                ..
            } => {
                for (_k, v) in fields {
                    visit(v, comments, by_line, source, diagnostics, false);
                }
                for child in body {
                    visit(child, comments, by_line, source, diagnostics, false);
                }
                if let Some(summary_body) = summarize {
                    for child in summary_body {
                        visit(child, comments, by_line, source, diagnostics, false);
                    }
                }
            }
            Node::ImplBlock { methods, .. } => {
                for m in methods {
                    visit(m, comments, by_line, source, diagnostics, false);
                }
            }
            _ => {}
        }
    }

    for node in program {
        visit(node, &comments, &by_line, source, diagnostics, true);
    }
}

fn check_one_item(
    node: &SNode,
    _comments: &[LegacyCommentTok],
    by_line: &std::collections::HashMap<usize, &LegacyCommentTok>,
    source: &str,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let item_line = node.span.line;
    if item_line == 0 {
        return;
    }
    // Contiguous run of wrong-format comments directly above the item. A
    // canonical `/** */` block stops the walk (already correct); `//` / `///`
    // lines and plain `/* */` blocks are collected for rewriting.
    let walked = migratable_run_above(by_line, item_line);
    if walked.is_empty() {
        return;
    }
    // Any contiguous run of wrong-format comments directly above the item
    // (no blank-line gap) is treated as its doc block.
    let any_doc = walked.iter().any(|c| c.is_doc);
    let any_plain = walked.iter().any(|c| !c.is_doc);
    let all_block = walked.iter().all(|c| !c.is_line);

    // Replacement span starts at the first comment's line_start so we can
    // reset indentation, and ends at the last comment's byte so the trailing
    // newline is left untouched.
    let first = walked.first().unwrap();
    let last = walked.last().unwrap();
    let line_start = line_start_byte(source, first.start_byte);
    let indent_cols = first.start_byte - line_start;
    let mut body_lines: Vec<String> = Vec::with_capacity(walked.len());
    for c in &walked {
        push_comment_body_lines(c, &mut body_lines);
    }
    let replacement = canonical_doc_block(&body_lines, indent_cols, 100);
    // Render the diagnostic/replacement at the run's *start* line so a
    // multi-line block comment's caret points where the comment begins.
    let start_line = first.start_line;
    let replace_span = Span::with_offsets(line_start, last.end_byte, start_line, 1);
    let fix = vec![FixEdit {
        span: replace_span,
        replacement,
    }];
    let (prefix, suggestion_form): (&str, &str) = if all_block {
        (
            "plain `/* */`",
            "/* */ block comment adjacent to the definition",
        )
    } else {
        match (any_doc, any_plain) {
            (true, false) => ("`///`", "/// lines"),
            (false, true) => ("plain `//`", "// lines adjacent to the definition"),
            _ => (
                "adjacent `//` / `///`",
                "line-comment block adjacent to the definition",
            ),
        }
    };
    diagnostics.push(LintDiagnostic {
        code: Code::LintLegacyDocComment,
        rule: "legacy-doc-comment".into(),
        message: format!("{prefix} doc comment(s) above this item should use `/** */` form"),
        span: Span::with_offsets(first.start_byte, last.end_byte, start_line, 1),
        severity: LintSeverity::Warning,
        suggestion: Some(format!(
            "rewrite the {suggestion_form} as a canonical `/** ... */` block"
        )),
        fix: Some(fix),
    });
}

/// Append the body text line(s) of a wrong-format comment token, stripped of
/// markers, to `out`. A line comment contributes one line; a plain block
/// comment contributes one line per physical line, with leading `*` / `* `
/// decoration removed so re-decoration by [`canonical_doc_block`] is clean.
fn push_comment_body_lines(tok: &LegacyCommentTok, out: &mut Vec<String>) {
    if tok.is_line {
        let s = tok.text.strip_prefix(' ').unwrap_or(&tok.text);
        out.push(s.trim_end().to_string());
        return;
    }
    for raw in tok.text.split('\n') {
        let t = raw.trim();
        let stripped = t
            .strip_prefix('*')
            .map(|s| s.strip_prefix(' ').unwrap_or(s))
            .unwrap_or(t);
        out.push(stripped.trim_end().to_string());
    }
}

/// Given a byte offset, walk backward to find the start-of-line byte.
fn line_start_byte(source: &str, offset: usize) -> usize {
    let bytes = source.as_bytes();
    let mut i = offset;
    while i > 0 && bytes[i - 1] != b'\n' {
        i -= 1;
    }
    i
}

pub(crate) fn extract_harndoc(source: &str, span: &Span) -> Option<String> {
    // Only canonical `/** */` doc blocks count here; legacy `///` forms are
    // handled by the `legacy-doc-comment` rule instead.
    let lines: Vec<&str> = source.lines().collect();
    let def_line = span.line.saturating_sub(1);
    if def_line == 0 {
        return None;
    }
    let above_idx = def_line - 1;
    let above = lines.get(above_idx)?.trim_end();
    if !above.ends_with("*/") {
        return None;
    }
    let above_trim = above.trim_start();
    if above_trim.starts_with("/**") && above_trim.ends_with("*/") && above_trim.len() >= 5 {
        let inner = &above_trim[3..above_trim.len() - 2];
        let text = inner.trim();
        return Some(text.to_string());
    }
    let mut start_idx = above_idx;
    loop {
        let cur = lines.get(start_idx)?.trim_start();
        if cur.starts_with("/**") {
            break;
        }
        if start_idx == 0 {
            return None;
        }
        start_idx -= 1;
    }
    let mut body: Vec<String> = Vec::with_capacity((above_idx + 1).saturating_sub(start_idx));
    for (i, line) in lines.iter().enumerate().take(above_idx + 1).skip(start_idx) {
        let t = line.trim();
        let stripped: &str = if i == start_idx {
            t.strip_prefix("/**").unwrap_or(t).trim_start()
        } else if i == above_idx {
            let without_tail = t.strip_suffix("*/").unwrap_or(t).trim_end();
            let without_star = without_tail
                .strip_prefix('*')
                .map(|s| s.strip_prefix(' ').unwrap_or(s))
                .unwrap_or(without_tail);
            without_star
        } else {
            t.strip_prefix('*')
                .map(|s| s.strip_prefix(' ').unwrap_or(s))
                .unwrap_or(t)
        };
        body.push(stripped.trim_end().to_string());
    }
    // Trim leading/trailing empty lines without O(n) shifts.
    let leading = body.iter().take_while(|s| s.is_empty()).count();
    let trailing = body.iter().rev().take_while(|s| s.is_empty()).count();
    if leading + trailing >= body.len() {
        return None;
    }
    let end = body.len() - trailing;
    let trimmed: Vec<String> = body.drain(leading..end).collect();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.join("\n"))
    }
}
