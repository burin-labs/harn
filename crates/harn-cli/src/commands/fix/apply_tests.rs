//! Tests for the edit-application layer: how planned edits are grouped per
//! file, and what `--apply` refuses to write.
//!
//! These sit apart from `tests.rs` because they exercise `apply.rs` directly
//! rather than the planner, and because a repair that writes unparseable
//! source is the one failure the rest of the suite cannot observe (#6148).

use super::*;
use std::fs;

#[test]
fn edit_group_key_collapses_relative_and_absolute_spellings() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("workflow.harn");
    fs::write(&script, "const value = 1\n").unwrap();

    // The per-file lint pass reports a relative path and the whole-program
    // capability pass reports an absolute one. Keyed on the raw string these
    // are two groups over one file, and the second group then applies spans
    // computed against source the first group already rewrote (#6148).
    let previous = std::env::current_dir().unwrap();
    std::env::set_current_dir(temp.path()).unwrap();
    let relative = edit_group_key("./workflow.harn");
    std::env::set_current_dir(previous).unwrap();

    let absolute = edit_group_key(&script.to_string_lossy());
    assert_eq!(
        relative, absolute,
        "both spellings of one file must group together"
    );
}

#[test]
fn edit_group_key_keeps_unresolvable_paths_verbatim() {
    let missing = "./does/not/exist.harn";
    assert_eq!(
        edit_group_key(missing),
        missing,
        "an unresolvable path keeps its spelling so the later read reports it"
    );
}

#[test]
fn apply_file_edits_refuses_to_write_unparseable_source() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("broken.harn");
    let original = "fn main(harness: Harness) {\n  const value = 1\n}\n";
    fs::write(&script, original).unwrap();

    // An edit landing mid-token is what a stale span produces. Writing that
    // out is never right, and the caller cannot detect it afterwards: an
    // unparseable file contributes no diagnostics, so the run reports clean.
    let edits = vec![FixEditWire {
        span: SpanWire::from(Span::with_offsets(10, 10, 1, 11)),
        replacement: "!!(".to_string(),
    }];
    let error = apply_file_edits(&script, &edits)
        .expect_err("a candidate that does not parse must be rejected");

    assert!(
        error.contains("invalid Harn syntax"),
        "error should name the syntax failure: {error}"
    );
    assert!(
        error.contains("applied edits:"),
        "error should list the edits so the bad span is visible: {error}"
    );
    assert_eq!(
        fs::read_to_string(&script).unwrap(),
        original,
        "the file must be left exactly as it was"
    );
}
