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
const CONTEXT: usize = 3;

/// A rendered line diff: the unified-diff hunk body plus raw `+`/`-` counts.
///
/// `body` carries `@@` hunk headers, up to [`CONTEXT`] context lines per side,
/// and `\ No newline at end of file` markers, but no file header — the caller
/// prepends that. It is empty exactly when the inputs are identical. A line
/// modified in place counts toward both `lines_added` and `lines_removed`.
pub struct LineDiff {
    pub body: String,
    pub lines_added: usize,
    pub lines_removed: usize,
}

/// Diff `before` against `after` line by line.
///
/// `from`/`to` are compared with trailing terminators intact, so a file that
/// only gains or loses its final newline still diffs as a change (and earns
/// the `\ No newline at end of file` marker) rather than collapsing to a no-op.
pub fn render_line_diff(before: &str, after: &str) -> LineDiff {
    let diff = TextDiff::configure()
        .algorithm(ALGORITHM)
        .diff_lines(before, after);
    let body = diff.unified_diff().context_radius(CONTEXT).to_string();

    let mut lines_added = 0;
    let mut lines_removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => lines_added += 1,
            ChangeTag::Delete => lines_removed += 1,
            ChangeTag::Equal => {}
        }
    }

    LineDiff {
        body,
        lines_added,
        lines_removed,
    }
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
