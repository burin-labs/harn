//! `blank-line-between-items` rule: top-level items should be separated
//! by at least one blank line, with any contiguous comment block above
//! an item treated as part of that item.

use harn_lexer::{FixEdit, Span};
use harn_parser::SNode;

use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::harndoc::{collect_comment_tokens, LegacyCommentTok};
use crate::naming::{build_line_starts, is_import_item, is_top_level_item};

/// Emit `blank-line-between-items` diagnostics. Doc comments immediately
/// preceding an item count as part of the item, so the blank line goes
/// above the doc block rather than between doc and item.
pub(crate) fn check_blank_line_between_items(
    source: &str,
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    if program.len() < 2 {
        return;
    }
    let comment_tokens = collect_comment_tokens(source);
    let comments_by_line: std::collections::HashMap<usize, &LegacyCommentTok> =
        comment_tokens.iter().map(|c| (c.line, c)).collect();

    let line_starts = build_line_starts(source);

    for pair in program.windows(2) {
        let prev = &pair[0];
        let next = &pair[1];

        // Consecutive imports intentionally stay tight.
        if is_import_item(&prev.node) && is_import_item(&next.node) {
            continue;
        }
        if !is_top_level_item(&prev.node) && !is_import_item(&prev.node) {
            continue;
        }
        if !is_top_level_item(&next.node) && !is_import_item(&next.node) {
            continue;
        }
        if prev.span.line == 0 || next.span.line == 0 {
            continue;
        }

        // Treat a contiguous comment block directly above `next` as part
        // of the item, so the blank line belongs above the doc block.
        let mut first_line = next.span.line;
        let mut probe = next.span.line;
        while probe > 1 {
            let above = probe - 1;
            if comments_by_line.contains_key(&above) {
                first_line = above;
                probe = above;
                continue;
            }
            break;
        }

        let prev_end_line = prev.span.end_line.max(prev.span.line);
        // Adjacent means zero blank lines between prev and the glued comment
        // block above next; insert a blank line on the line after prev.
        if first_line <= prev_end_line + 1 {
            let insert_line = prev_end_line + 1;
            let Some(&insert_offset) = line_starts.get(insert_line.saturating_sub(1)) else {
                continue;
            };
            let span = Span::with_offsets(insert_offset, insert_offset, insert_line, 1);
            diagnostics.push(LintDiagnostic {
                rule: "blank-line-between-items",
                message: "top-level items should be separated by a blank line".to_string(),
                span,
                severity: LintSeverity::Warning,
                suggestion: Some(
                    "insert a blank line above the next item (doc comments \
                     stay glued to the item they describe)"
                        .to_string(),
                ),
                fix: Some(vec![FixEdit {
                    span,
                    replacement: "\n".to_string(),
                }]),
            });
        }
    }
}
