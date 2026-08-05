//! Rendering a unified diff.
//!
//! Additions, removals, insertions and deletions in the middle, the degenerate
//! empty/identical/all-changed inputs, insert-only and delete-only runs, and a
//! trailing-newline change staying visible.

use crate::orchestration::*;
#[test]
fn render_unified_diff_marks_removed_and_added_lines() {
    let diff = render_unified_diff(Some("src/main.rs"), "old\nsame", "new\nsame");
    assert!(diff.contains("--- a/src/main.rs"));
    assert!(diff.contains("+++ b/src/main.rs"));
    assert!(diff.contains("-old"));
    assert!(diff.contains("+new"));
    assert!(diff.contains(" same"));
}

#[test]
fn render_unified_diff_identical_inputs() {
    let text = "line1\nline2\nline3";
    // No hunks means no diff at all — not a bare header over every line.
    assert_eq!(render_unified_diff(None, text, text), "");
}

#[test]
fn render_unified_diff_empty_before() {
    let diff = render_unified_diff(None, "", "new1\nnew2");
    assert!(diff.contains("+new1"));
    assert!(diff.contains("+new2"));
    let body: Vec<&str> = diff.lines().skip(2).collect();
    assert!(!body.iter().any(|l| l.starts_with('-')));
}

#[test]
fn render_unified_diff_empty_after() {
    let diff = render_unified_diff(None, "old1\nold2", "");
    assert!(diff.contains("-old1"));
    assert!(diff.contains("-old2"));
    let body: Vec<&str> = diff.lines().skip(2).collect();
    assert!(!body.iter().any(|l| l.starts_with('+')));
}

#[test]
fn render_unified_diff_both_empty() {
    assert_eq!(render_unified_diff(None, "", ""), "");
}

#[test]
fn render_unified_diff_all_changed() {
    let diff = render_unified_diff(None, "a\nb", "x\ny");
    assert!(diff.contains("-a"));
    assert!(diff.contains("-b"));
    assert!(diff.contains("+x"));
    assert!(diff.contains("+y"));
}

#[test]
fn render_unified_diff_insertion_in_middle() {
    let diff = render_unified_diff(None, "a\nc", "a\nb\nc");
    assert!(diff.contains(" a"));
    assert!(diff.contains("+b"));
    assert!(diff.contains(" c"));
    let body: Vec<&str> = diff.lines().skip(2).collect();
    assert!(!body.iter().any(|l| l.starts_with('-')));
}

#[test]
fn render_unified_diff_deletion_from_middle() {
    let diff = render_unified_diff(None, "a\nb\nc", "a\nc");
    assert!(diff.contains(" a"));
    assert!(diff.contains("-b"));
    assert!(diff.contains(" c"));
    let body: Vec<&str> = diff.lines().skip(2).collect();
    assert!(!body.iter().any(|l| l.starts_with('+')));
}

#[test]
fn render_unified_diff_default_path() {
    let diff = render_unified_diff(None, "a", "b");
    assert!(diff.contains("--- a/artifact"));
    assert!(diff.contains("+++ b/artifact"));
}

#[test]
fn render_unified_diff_large_similar() {
    let mut before = Vec::new();
    let mut after = Vec::new();
    for i in 0..1000 {
        before.push(format!("line {i}"));
        after.push(format!("line {i}"));
    }
    before[500] = "OLD LINE 500".to_string();
    after[500] = "NEW LINE 500".to_string();
    let before_str = before.join("\n");
    let after_str = after.join("\n");
    let diff = render_unified_diff(None, &before_str, &after_str);
    assert!(diff.contains("-OLD LINE 500"));
    assert!(diff.contains("+NEW LINE 500"));
    assert!(diff.contains(" line 499"));
    assert!(diff.contains(" line 501"));
    // A hunk header locates the change, and context stays bounded rather
    // than reprinting all 1000 unchanged lines.
    assert!(diff.contains("@@ -"));
    assert!(!diff.contains(" line 100\n"));
    assert!(diff.lines().count() < 20);
}

#[test]
fn render_unified_diff_insert_only_has_no_removals() {
    let diff = render_unified_diff(None, "", "a\nb");
    let body: Vec<&str> = diff.lines().skip(3).collect();
    assert_eq!(body.iter().filter(|l| l.starts_with('+')).count(), 2);
    assert!(!body.iter().any(|l| l.starts_with('-')));
}

#[test]
fn render_unified_diff_delete_only_has_no_insertions() {
    let diff = render_unified_diff(None, "a\nb", "");
    let body: Vec<&str> = diff.lines().skip(3).collect();
    assert_eq!(body.iter().filter(|l| l.starts_with('-')).count(), 2);
    assert!(!body.iter().any(|l| l.starts_with('+')));
}

#[test]
fn render_unified_diff_trailing_newline_change_is_visible() {
    // "a\nb" and "a\nb\n" differ only in the final byte; the diff must
    // not collapse them into a no-op.
    let diff = render_unified_diff(None, "a\nb", "a\nb\n");
    assert!(diff.contains("\\ No newline at end of file"));
    assert!(diff.contains("-b"));
    assert!(diff.contains("+b"));
}
