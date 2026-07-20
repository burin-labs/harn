//! Unified-diff renderer used by the `ast.dry_run` preview path.
//!
//! Produces `git apply --check`-compatible output: hunk headers
//! (`@@ -a,b +c,d @@`), leading and trailing context per hunk, and
//! `\ No newline at end of file` markers when either side lacks a trailing
//! newline. The line diff, hunk grouping, and `+`/`-` counts come from the
//! workspace's shared [`harn_vm::text_diff`] owner; this module only supplies
//! the file header.

use harn_vm::text_diff::render_line_diff;

/// Per-file diff with summary counts. `lines_added` and `lines_removed`
/// are raw `+`/`-` line counts (a modified line shows up as both).
pub(super) struct FileDiff {
    pub(super) text: String,
    pub(super) lines_added: usize,
    pub(super) lines_removed: usize,
}

/// Render a unified diff for one file going from `before` to `after`.
///
/// `path` becomes the `a/...` and `b/...` label. Either side being empty
/// is reported as creation (`--- /dev/null`) or deletion (`+++ /dev/null`).
/// Identical inputs produce no body (empty `FileDiff::text`).
pub(super) fn render(path: &str, before: &str, after: &str, kind: ChangeKind) -> FileDiff {
    let diff = render_line_diff(before, after);
    if diff.body.is_empty() {
        return FileDiff {
            text: String::new(),
            lines_added: 0,
            lines_removed: 0,
        };
    }

    let header = match kind {
        ChangeKind::Modify => format!("--- a/{path}\n+++ b/{path}\n"),
        ChangeKind::Create => format!("--- /dev/null\n+++ b/{path}\n"),
        ChangeKind::Delete => format!("--- a/{path}\n+++ /dev/null\n"),
    };

    FileDiff {
        text: header + &diff.body,
        lines_added: diff.lines_added,
        lines_removed: diff.lines_removed,
    }
}

/// File-level change kind. Drives the header line shape.
#[derive(Clone, Copy)]
pub(super) enum ChangeKind {
    Modify,
    Create,
    Delete,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_change_yields_empty_diff() {
        let out = render(
            "foo.txt",
            "alpha\nbeta\n",
            "alpha\nbeta\n",
            ChangeKind::Modify,
        );
        assert!(out.text.is_empty());
        assert_eq!(out.lines_added, 0);
        assert_eq!(out.lines_removed, 0);
    }

    #[test]
    fn single_line_modification_emits_hunk_header() {
        let out = render(
            "src/lib.rs",
            "fn alpha() { 1 }\nfn beta() { 2 }\n",
            "fn alpha() { 1 }\nfn beta() { 42 }\n",
            ChangeKind::Modify,
        );
        assert!(out.text.contains("--- a/src/lib.rs"));
        assert!(out.text.contains("+++ b/src/lib.rs"));
        assert!(out.text.contains("@@ -"));
        assert!(out.text.contains("-fn beta() { 2 }"));
        assert!(out.text.contains("+fn beta() { 42 }"));
        assert_eq!(out.lines_added, 1);
        assert_eq!(out.lines_removed, 1);
    }

    #[test]
    fn file_creation_uses_dev_null_a() {
        let out = render("new.txt", "", "hello\n", ChangeKind::Create);
        assert!(out.text.starts_with("--- /dev/null\n+++ b/new.txt\n"));
        assert!(out.text.contains("+hello"));
        assert_eq!(out.lines_added, 1);
        assert_eq!(out.lines_removed, 0);
    }

    #[test]
    fn file_deletion_uses_dev_null_b() {
        let out = render("old.txt", "bye\n", "", ChangeKind::Delete);
        assert!(out.text.starts_with("--- a/old.txt\n+++ /dev/null\n"));
        assert!(out.text.contains("-bye"));
        assert_eq!(out.lines_removed, 1);
        assert_eq!(out.lines_added, 0);
    }

    #[test]
    fn missing_trailing_newline_emits_marker() {
        let out = render("f.txt", "alpha\nbeta", "alpha\nbeta\n", ChangeKind::Modify);
        // The before side lacks a trailing newline; the marker must
        // appear on the `-beta` removal so `git apply` recovers the
        // original byte sequence.
        assert!(out.text.contains("-beta\n\\ No newline at end of file\n"));
    }

    #[test]
    fn hunk_with_context_marks_three_surrounding_lines() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
        let after = "a\nb\nc\nD\ne\nf\ng\nh\ni\nj\n";
        let out = render("f.txt", before, after, ChangeKind::Modify);
        // Three lines before the change and three after — ten total
        // lines but only seven appear in the hunk plus the modified pair.
        let body = out.text.lines().collect::<Vec<_>>();
        assert!(body.iter().any(|l| l.starts_with(" a")));
        assert!(body.iter().any(|l| l.starts_with(" g")));
        assert!(!body.iter().any(|l| l.starts_with(" j")));
        assert!(body.contains(&"-d"));
        assert!(body.contains(&"+D"));
    }

    #[test]
    fn distant_changes_produce_separate_hunks() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
        let after = "a\nB\nc\nd\ne\nf\ng\nh\ni\nj\nk\nL\n";
        let out = render("f.txt", before, after, ChangeKind::Modify);
        let hunk_headers = out.text.matches("@@ -").count();
        assert_eq!(hunk_headers, 2);
    }
}
