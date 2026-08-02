//! The workspace's single line-diff owner.
//!
//! Every unified-diff renderer in the codebase — orchestration run-record
//! diffs, the `ast.dry_run` preview, and the `harn package publish` index
//! preview — routes through [`render_line_diff`] so the algorithm choice and
//! context radius live in exactly one place. Callers supply their own file
//! header (`--- a/… / +++ b/…`, `/dev/null`, etc.); this module owns the
//! `@@` hunk body and the `+`/`-` line counts.

use similar::{Algorithm, ChangeTag, TextDiff};

/// Line-diff algorithm shared by every renderer.
///
/// Histogram is anchor-based: it produces more stable, human-readable hunks
/// on real source than plain Myers, at equal correctness and generally lower
/// cost. Every consumer here renders diffs for people to read (run
/// comparisons, edit previews, index-change previews), so hunk quality is
/// worth more than matching Myers' particular edit script.
const ALGORITHM: Algorithm = Algorithm::Histogram;

/// Unchanged lines kept on each side of a hunk. Matches `git diff`'s default.
pub(crate) const DEFAULT_CONTEXT: usize = 3;

/// A rendered line diff: the unified-diff hunk body plus raw `+`/`-` counts.
///
/// `body` carries `@@` hunk headers, up to [`DEFAULT_CONTEXT`] context lines per side,
/// and `\ No newline at end of file` markers, but no file header — the caller
/// prepends that. It is empty exactly when the inputs are identical. A line
/// modified in place counts toward both `lines_added` and `lines_removed`.
pub struct LineDiff {
    pub body: String,
    pub lines_added: usize,
    pub lines_removed: usize,
    pub old_lines: usize,
    pub new_lines: usize,
    pub changes: Vec<LineChange>,
}

/// One line in an expanded edit script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineChangeKind {
    Equal,
    Delete,
    Insert,
}

impl LineChangeKind {
    /// Stable wire spelling used by `std/diff`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::Delete => "delete",
            Self::Insert => "insert",
        }
    }
}

/// Expanded line operation with one-based coordinates on both sides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineChange {
    pub kind: LineChangeKind,
    pub line: String,
    pub old_line: usize,
    pub new_line: usize,
}

/// Work requested from the shared line-diff engine.
#[derive(Debug, Clone, Copy)]
pub struct LineDiffOptions {
    pub context: usize,
    pub include_body: bool,
    pub include_changes: bool,
}

impl Default for LineDiffOptions {
    fn default() -> Self {
        Self {
            context: DEFAULT_CONTEXT,
            include_body: true,
            include_changes: false,
        }
    }
}

/// Diff `before` against `after` line by line.
///
/// `from`/`to` are compared with trailing terminators intact, so a file that
/// only gains or loses its final newline still diffs as a change (and earns
/// the `\ No newline at end of file` marker) rather than collapsing to a no-op.
pub fn render_line_diff(before: &str, after: &str) -> LineDiff {
    compute_line_diff(before, after, LineDiffOptions::default())
}

/// Compute one line diff and project only the representations a caller needs.
pub fn compute_line_diff(before: &str, after: &str, options: LineDiffOptions) -> LineDiff {
    let diff = TextDiff::configure()
        .algorithm(ALGORITHM)
        .diff_lines(before, after);
    let body = if options.include_body {
        diff.unified_diff()
            .context_radius(options.context)
            .to_string()
    } else {
        String::new()
    };

    let mut lines_added = 0;
    let mut lines_removed = 0;
    let mut old_line = 1;
    let mut new_line = 1;
    let mut changes = if options.include_changes {
        Vec::with_capacity(diff.old_len().max(diff.new_len()))
    } else {
        Vec::new()
    };
    for change in diff.iter_all_changes() {
        let kind = match change.tag() {
            ChangeTag::Insert => {
                lines_added += 1;
                LineChangeKind::Insert
            }
            ChangeTag::Delete => {
                lines_removed += 1;
                LineChangeKind::Delete
            }
            ChangeTag::Equal => LineChangeKind::Equal,
        };
        if options.include_changes {
            changes.push(LineChange {
                kind,
                line: line_without_terminator(change.value()),
                old_line,
                new_line,
            });
        }
        match kind {
            LineChangeKind::Equal => {
                old_line += 1;
                new_line += 1;
            }
            LineChangeKind::Delete => old_line += 1,
            LineChangeKind::Insert => new_line += 1,
        }
    }

    LineDiff {
        body,
        lines_added,
        lines_removed,
        old_lines: diff.old_len(),
        new_lines: diff.new_len(),
        changes,
    }
}

fn line_without_terminator(value: &str) -> String {
    let value = value.strip_suffix('\n').unwrap_or(value);
    value.strip_suffix('\r').unwrap_or(value).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_inputs_yield_empty_body() {
        let diff = render_line_diff("a\nb\nc\n", "a\nb\nc\n");
        assert_eq!(diff.body, "");
        assert_eq!(diff.lines_added, 0);
        assert_eq!(diff.lines_removed, 0);
    }

    #[test]
    fn single_change_emits_bounded_hunk() {
        let diff = render_line_diff("a\nb\nc\n", "a\nB\nc\n");
        assert!(diff.body.starts_with("@@ -"));
        assert!(diff.body.contains("-b\n"));
        assert!(diff.body.contains("+B\n"));
        assert_eq!(diff.lines_added, 1);
        assert_eq!(diff.lines_removed, 1);
    }

    #[test]
    fn expanded_changes_keep_one_based_coordinates() {
        let diff = compute_line_diff(
            "a\nb\nc\n",
            "a\nB\nc\n",
            LineDiffOptions {
                include_body: false,
                include_changes: true,
                ..LineDiffOptions::default()
            },
        );
        assert_eq!(diff.old_lines, 3);
        assert_eq!(diff.new_lines, 3);
        assert_eq!(diff.changes[1].kind, LineChangeKind::Delete);
        assert_eq!(diff.changes[1].line, "b");
        assert_eq!((diff.changes[1].old_line, diff.changes[1].new_line), (2, 2));
        assert_eq!(diff.changes[2].kind, LineChangeKind::Insert);
        assert_eq!(diff.changes[2].line, "B");
        assert_eq!((diff.changes[2].old_line, diff.changes[2].new_line), (3, 2));
    }

    #[test]
    fn context_stays_bounded_on_large_inputs() {
        let before: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let mut after_lines: Vec<String> = (0..1000).map(|i| format!("line {i}")).collect();
        after_lines[500] = "CHANGED".to_string();
        let after = after_lines
            .iter()
            .map(|l| format!("{l}\n"))
            .collect::<String>();
        let diff = render_line_diff(&before, &after);
        // One hunk with 3 lines of context each side — not all 1000 lines.
        assert_eq!(diff.body.matches("@@ -").count(), 1);
        assert!(diff.body.lines().count() < 12);
        assert!(!diff.body.contains("line 100\n"));
    }

    #[test]
    fn trailing_newline_change_is_not_collapsed() {
        let diff = render_line_diff("a\nb", "a\nb\n");
        assert!(diff.body.contains("\\ No newline at end of file"));
    }
}
