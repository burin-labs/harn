//! Tests for harndoc requirements on public APIs and the
//! legacy-doc-comment migration rule.

use super::*;

#[test]
fn test_clean_code() {
    let diags = lint_source(
        r#"
pipeline default(task) {
let x = 1
log(x)
}
"#,
    );
    // x is used, task is a pipeline param -- should be clean.
    assert!(
        !has_rule(&diags, "unused-variable"),
        "expected no unused-variable, got: {diags:?}"
    );
}

#[test]
fn test_public_function_requires_harndoc() {
    let diags = lint_source(
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
    let diags = lint_source(
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
fn test_plain_comment_does_not_satisfy_harndoc() {
    let diags = lint_source(
        r#"
// Not HarnDoc.
pub fn exposed() -> string {
  return "x"
}
"#,
    );
    assert!(has_rule(&diags, "missing-harndoc"));
}

#[test]
fn test_private_function_does_not_require_harndoc() {
    let diags = lint_source(
        r#"
fn helper() -> string {
  return "x"
}
"#,
    );
    assert!(!has_rule(&diags, "missing-harndoc"));
}
