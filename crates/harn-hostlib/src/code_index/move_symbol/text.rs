//! Byte-level edit plumbing shared by every language arm of `move_symbol`.

use std::ops::Range;

use tree_sitter::Node;

use crate::code_index::refactor_core::{
    is_interpolation_kind, is_skip_kind, EditSpan, IdentifierSpan,
};

/// One planned replacement in one file. Zero-width ranges are insertions;
/// `order` sorts insertions that land on the same byte.
#[derive(Clone, Debug)]
pub(crate) struct Edit {
    pub range: Range<usize>,
    pub text: String,
    pub order: u8,
}

impl Edit {
    pub(crate) fn replace(range: Range<usize>, text: impl Into<String>) -> Self {
        Self {
            range,
            text: text.into(),
            order: 1,
        }
    }

    pub(crate) fn insert(at: usize, text: impl Into<String>, order: u8) -> Self {
        Self {
            range: at..at,
            text: text.into(),
            order,
        }
    }
}

/// Order edits, fold same-offset insertions into one span, and refuse
/// overlaps. The result splices cleanly with `refactor_core::splice`.
pub(crate) fn finalize(source: &str, mut edits: Vec<Edit>) -> Result<Vec<EditSpan>, String> {
    edits.sort_by_key(|edit| {
        (
            edit.range.start,
            edit.range.end != edit.range.start,
            edit.order,
        )
    });
    let mut merged: Vec<Edit> = Vec::new();
    for edit in edits {
        if let Some(last) = merged.last_mut() {
            if last.range.start == edit.range.start && last.range.is_empty() {
                // Insertions at one offset concatenate in order; a following
                // replacement at the same offset absorbs them as a prefix.
                last.range.end = edit.range.end;
                last.text.push_str(&edit.text);
                continue;
            }
            if edit.range.start < last.range.end {
                return Err(format!(
                    "planned edits overlap at bytes {}..{} and {}..{}",
                    last.range.start, last.range.end, edit.range.start, edit.range.end
                ));
            }
        }
        merged.push(edit);
    }
    Ok(merged
        .into_iter()
        .map(|edit| EditSpan {
            span: span_of(source, edit.range.clone()),
            before: slice(source, edit.range.clone()).to_string(),
            after: edit.text,
        })
        .collect())
}

/// Byte range plus 0-based row/column (byte columns, like tree-sitter).
pub(crate) fn span_of(source: &str, range: Range<usize>) -> IdentifierSpan {
    let (start_row, start_col) = row_col(source, range.start);
    let (end_row, end_col) = row_col(source, range.end);
    IdentifierSpan {
        start_byte: range.start,
        end_byte: range.end,
        start_row,
        start_col,
        end_row,
        end_col,
    }
}

fn row_col(source: &str, at: usize) -> (usize, usize) {
    let before = &source.as_bytes()[..at];
    let row = before.split(|b| *b == b'\n').count() - 1;
    let col = at - line_start(source, at);
    (row, col)
}

/// `source[range]` for offsets that come from tree-sitter nodes or line
/// scans, which always sit on character boundaries.
pub(crate) fn slice<R: std::slice::SliceIndex<str, Output = str>>(source: &str, range: R) -> &str {
    source.get(range).unwrap_or_default()
}

pub(crate) fn line_start(source: &str, at: usize) -> usize {
    slice(source, ..at).rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// Offset just past the newline ending the line that holds `at`.
pub(crate) fn next_line_start(source: &str, at: usize) -> usize {
    slice(source, at..)
        .find('\n')
        .map(|i| at + i + 1)
        .unwrap_or(source.len())
}

/// 1-based line number of a byte offset.
pub(crate) fn line_of(source: &str, at: usize) -> u32 {
    row_col(source, at).0 as u32 + 1
}

pub(crate) fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    slice(source, node.byte_range())
}

/// Widen `range` to whole lines when nothing but whitespace shares them,
/// so deleting a statement leaves no blank husk.
pub(crate) fn whole_lines(source: &str, range: Range<usize>) -> Range<usize> {
    let start = line_start(source, range.start);
    let end = next_line_start(source, range.end);
    let before_blank = slice(source, start..range.start).trim().is_empty();
    let after_blank = slice(source, range.end..end).trim().is_empty();
    if before_blank && after_blank {
        start..end
    } else {
        range
    }
}

/// Range that removes `elements[index]` from a comma-separated list along
/// with exactly one separating comma. Callers handle the sole-element case.
pub(crate) fn list_element_removal(elements: &[Range<usize>], index: usize) -> Range<usize> {
    if index + 1 < elements.len() {
        elements[index].start..elements[index + 1].start
    } else {
        elements[index - 1].end..elements[index].end
    }
}

/// Named children that are list members rather than comments.
pub(crate) fn members<'tree>(node: Node<'tree>) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| !child.kind().contains("comment"))
        .collect()
}

pub(crate) fn is_field_child(parent: Node<'_>, field: &str, node: Node<'_>) -> bool {
    parent.child_by_field_name(field).map(|c| c.id()) == Some(node.id())
}

/// Visit every node under `root`, depth first.
/// Visit the code under `root`, depth first, as refactor_core's identifier
/// descent sees it: comments and literal text are not visited, but code
/// interpolated into a string (`f"{x}"`, `${x}`) is. `visit` returning false
/// prunes a node's children.
pub(crate) fn walk<'tree>(root: Node<'tree>, mut visit: impl FnMut(Node<'tree>) -> bool) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if is_skip_kind(node.kind()) {
            let mut cursor = node.walk();
            stack.extend(
                node.children(&mut cursor)
                    .filter(|child| is_interpolation_kind(child.kind())),
            );
            continue;
        }
        if !visit(node) {
            continue;
        }
        let mut cursor = node.walk();
        let children: Vec<Node<'tree>> = node.children(&mut cursor).collect();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
}

/// Lines a newly inserted block needs so `want` newlines separate it from
/// the text before `at` (one blank line is `want == 2`).
pub(crate) fn separator_before(source: &str, at: usize, want: usize) -> String {
    if slice(source, ..at).trim().is_empty() {
        return String::new();
    }
    let have = slice(source, ..at)
        .chars()
        .rev()
        .take_while(|c| *c == '\n' || *c == ' ' || *c == '\t' || *c == '\r')
        .filter(|c| *c == '\n')
        .count();
    "\n".repeat(want.saturating_sub(have))
}
