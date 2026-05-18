//! `missing-stdlib-metadata` (HARN-STD-101) coverage.

use super::*;

#[test]
fn defers_to_missing_harndoc_when_no_block() {
    // No `/** */` block at all → the existing `missing-harndoc` lint
    // (HARN-LNT-024) covers it; HARN-STD-101 stays silent to avoid
    // double-reporting.
    let source = "pub fn foo() {\n  return 1\n}\n";
    let diags = lint_with_stdlib_metadata(source);
    assert!(
        has_rule(&diags, "missing-harndoc"),
        "expected missing-harndoc to fire, got: {diags:?}"
    );
    assert!(
        !has_rule(&diags, "missing-stdlib-metadata"),
        "HARN-STD-101 should defer to missing-harndoc when no block exists: {diags:?}"
    );
}

#[test]
fn warns_when_empty_block_present() {
    let source = "\
/**
 * Just prose.
 */
pub fn foo() {
  return 1
}
";
    let diags = lint_with_stdlib_metadata(source);
    let missing = diags
        .iter()
        .find(|d| d.rule == "missing-stdlib-metadata")
        .expect("should warn");
    assert!(
        missing.message.contains("metadata block"),
        "expected full-block message, got: {}",
        missing.message
    );
}

#[test]
fn warns_when_block_omits_a_field() {
    let source = "\
/**
 * Document.
 *
 * @effects: [fs.read]
 * @allocation: heap
 * @errors: []
 * @api_stability: stable
 */
pub fn foo() {
  return 1
}
";
    let diags = lint_with_stdlib_metadata(source);
    let missing = diags
        .iter()
        .find(|d| d.rule == "missing-stdlib-metadata")
        .expect("should warn");
    assert!(
        missing.message.contains("example"),
        "expected `example` listed as missing, got: {}",
        missing.message
    );
}

#[test]
fn accepts_complete_block() {
    let source = "\
/**
 * Document.
 *
 * @effects: [fs.read]
 * @allocation: heap
 * @errors: []
 * @api_stability: stable
 * @example: foo()
 */
pub fn foo() {
  return 1
}
";
    let diags = lint_with_stdlib_metadata(source);
    assert!(
        !has_rule(&diags, "missing-stdlib-metadata"),
        "complete metadata should not trip HARN-STD-101: {diags:?}"
    );
}

#[test]
fn does_not_fire_for_private_helpers() {
    let source = "\
fn helper() {
  return 1
}
";
    let diags = lint_with_stdlib_metadata(source);
    assert!(
        !has_rule(&diags, "missing-stdlib-metadata"),
        "private helpers are exempt: {diags:?}"
    );
}

#[test]
fn dormant_outside_stdlib_path() {
    // Without `require_stdlib_metadata: true` (i.e. the default lint
    // invocation user scripts hit), HARN-STD-101 is dormant.
    let source = "pub fn foo() {\n  return 1\n}\n";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "missing-stdlib-metadata"),
        "default lint must not enforce stdlib metadata: {diags:?}"
    );
}
