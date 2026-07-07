//! Tests for harndoc requirements on public APIs and the
//! legacy-doc-comment migration rule.

use super::*;

#[test]
fn test_clean_code() {
    let diags = lint_source(
        r"
pipeline default(task) {
const x = 1
log(x)
}
",
    );
    // x is used, task is a pipeline param -- should be clean.
    assert!(
        !has_rule(&diags, "unused-variable"),
        "expected no unused-variable, got: {diags:?}"
    );
}

#[test]
fn test_missing_harndoc_is_off_by_default() {
    // SOTA default: doc-presence lints are opt-in (`[lint]
    // require_docstrings = true`), so a bare `pub fn` is clean out of
    // the box.
    let diags = lint_source(
        r#"
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(
        !has_rule(&diags, "missing-harndoc"),
        "missing-harndoc must not fire by default: {diags:?}"
    );
}

#[test]
fn test_public_function_requires_harndoc_when_opted_in() {
    let diags = lint_with_docstrings(
        r#"
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(has_rule(&diags, "missing-harndoc"));
}

#[test]
fn test_stdlib_metadata_mode_implies_harndoc_requirement() {
    // The stdlib gate must keep missing-harndoc armed: HARN-STD-101
    // defers to it when no doc block exists at all.
    let diags = lint_with_stdlib_metadata(
        r#"
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(has_rule(&diags, "missing-harndoc"));
}

#[test]
fn test_public_function_with_harndoc_is_clean() {
    let diags = lint_with_docstrings(
        r#"
/** Explain the public API. */
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(!has_rule(&diags, "missing-harndoc"));
}

#[test]
fn test_public_function_with_multiline_harndoc_is_clean() {
    let diags = lint_source(
        r#"
/**
 * Explain the public API.
 * Across multiple lines.
 */
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(!has_rule(&diags, "missing-harndoc"));
}

#[test]
fn test_legacy_triple_slash_above_pub_fn_fires() {
    let diags = lint_source(
        r#"
/// Old-style doc.
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(
        has_rule(&diags, "legacy-doc-comment"),
        "expected legacy-doc-comment, got: {diags:?}"
    );
    // And the autofix should produce a canonical /** */ block.
    let fix = diags
        .iter()
        .find(|d| d.rule == "legacy-doc-comment")
        .and_then(|d| d.fix.as_ref())
        .expect("legacy-doc-comment must carry an autofix");
    assert_eq!(fix.len(), 1);
    assert!(
        fix[0].replacement.contains("/**") && fix[0].replacement.contains("*/"),
        "replacement should be a canonical /** */ block: {:?}",
        fix[0].replacement
    );
}

#[test]
fn test_plain_double_slash_adjacent_to_pub_fn_fires() {
    let diags = lint_source(
        r#"
// Doc-by-adjacency.
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(
        has_rule(&diags, "legacy-doc-comment"),
        "expected legacy-doc-comment for // adjacent to def, got: {diags:?}"
    );
}

#[test]
fn test_plain_double_slash_with_blank_line_does_not_fire() {
    let diags = lint_source(
        r#"
// unrelated comment

pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(
        !has_rule(&diags, "legacy-doc-comment"),
        "// with blank-line gap should not be treated as doc: {diags:?}"
    );
}

#[test]
fn test_existing_block_doc_does_not_fire_legacy() {
    let diags = lint_source(
        r#"
/** Already canonical. */
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(
        !has_rule(&diags, "legacy-doc-comment"),
        "/** */ block should not trigger legacy rule: {diags:?}"
    );
    assert!(
        !has_rule(&diags, "missing-harndoc"),
        "/** */ block should satisfy missing-harndoc: {diags:?}"
    );
}

#[test]
fn test_legacy_line_comment_suppresses_missing_harndoc() {
    // A wrong-format `//` doc comment is migratable, so the user should see
    // only the auto-fixable `legacy-doc-comment` finding, not the fixless
    // `missing-harndoc` alongside it.
    let diags = lint_with_docstrings(
        r#"
// Not HarnDoc.
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(
        has_rule(&diags, "legacy-doc-comment"),
        "expected legacy-doc-comment, got: {diags:?}"
    );
    assert!(
        !has_rule(&diags, "missing-harndoc"),
        "missing-harndoc should be suppressed when a migratable comment exists: {diags:?}"
    );
}

#[test]
fn test_triple_slash_suppresses_missing_harndoc() {
    let diags = lint_with_docstrings(
        r#"
/// Old-style doc.
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(has_rule(&diags, "legacy-doc-comment"));
    assert!(
        !has_rule(&diags, "missing-harndoc"),
        "missing-harndoc should be suppressed for `///`: {diags:?}"
    );
}

#[test]
fn test_plain_block_comment_above_pub_fn_migrates() {
    let diags = lint_with_docstrings(
        r#"
/* Not a doc block. */
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(
        has_rule(&diags, "legacy-doc-comment"),
        "plain /* */ block above a pub fn should migrate: {diags:?}"
    );
    assert!(
        !has_rule(&diags, "missing-harndoc"),
        "missing-harndoc should be suppressed for a migratable /* */ block: {diags:?}"
    );
    let fix = diags
        .iter()
        .find(|d| d.rule == "legacy-doc-comment")
        .and_then(|d| d.fix.as_ref())
        .expect("legacy-doc-comment must carry an autofix");
    assert_eq!(fix.len(), 1);
    assert!(
        fix[0].replacement.contains("/**") && fix[0].replacement.contains("*/"),
        "replacement should be a canonical /** */ block: {:?}",
        fix[0].replacement
    );
}

#[test]
fn test_multiline_block_comment_above_pub_fn_migrates() {
    let diags = lint_source(
        r#"
/* First line.
   Second line. */
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    let fix = diags
        .iter()
        .find(|d| d.rule == "legacy-doc-comment")
        .and_then(|d| d.fix.as_ref())
        .expect("multi-line /* */ block must migrate with a fix");
    assert_eq!(fix.len(), 1);
    let replacement = &fix[0].replacement;
    assert!(
        replacement.contains("/**") && replacement.contains("*/"),
        "replacement should be a canonical block: {replacement:?}"
    );
    assert!(
        replacement.contains("First line.") && replacement.contains("Second line."),
        "replacement should preserve both body lines: {replacement:?}"
    );
}

#[test]
fn test_block_comment_with_blank_gap_does_not_migrate() {
    let diags = lint_with_docstrings(
        r#"
/* unrelated */

pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(
        !has_rule(&diags, "legacy-doc-comment"),
        "/* */ with a blank-line gap should not be treated as doc: {diags:?}"
    );
    // With no adjacent migratable comment, the canonical doc is genuinely
    // missing, so `missing-harndoc` still fires.
    assert!(
        has_rule(&diags, "missing-harndoc"),
        "missing-harndoc should fire when the gapped comment is not adjacent: {diags:?}"
    );
}

#[test]
fn test_private_function_does_not_require_harndoc() {
    let diags = lint_with_docstrings(
        r#"
fn helper() -> string {
  return "x"
}
"#,
    );
    assert!(!has_rule(&diags, "missing-harndoc"));
}
