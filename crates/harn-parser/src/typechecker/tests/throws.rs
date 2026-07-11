//! HARN-TYP-026 — the `throws E` declared-exception-channel check.
//!
//! Deterministic, in-process typechecker assertions on the diagnostic CODE
//! (never prose/timing): a callable whose `throw` sites all conform to its
//! declared `throws` set checks clean; a `throw` of a type the set does not
//! cover raises TYP-026; and a callable with no `throws` clause is never
//! constrained (the annotation is additive). Catch-exhaustiveness of the
//! declared set is a separate, deferred check and is intentionally not tested
//! here.

use super::*;
use crate::diagnostic_codes::Code;

fn throws_mismatches(source: &str) -> Vec<String> {
    check_source(source)
        .into_iter()
        .filter(|d| d.code == Code::ThrowsTypeMismatch)
        .map(|d| d.message)
        .collect()
}

#[test]
fn throwing_the_declared_type_checks_clean() {
    let diags = throws_mismatches(
        r#"fn parse(s: string) throws string {
  throw "bad input"
}"#,
    );
    assert!(
        diags.is_empty(),
        "throwing the declared type must not raise TYP-026: {diags:?}"
    );
}

#[test]
fn throwing_an_undeclared_type_errors() {
    let diags = throws_mismatches(
        r#"fn parse(s: string) throws string {
  throw 42
}"#,
    );
    assert_eq!(
        diags.len(),
        1,
        "throwing a type outside the declared set must raise exactly one TYP-026: {diags:?}"
    );
}

#[test]
fn union_throws_set_covers_each_member() {
    let diags = throws_mismatches(
        r#"fn parse(s: string) throws (string | int) {
  if s == "" {
    throw 42
  }
  throw "bad"
}"#,
    );
    assert!(
        diags.is_empty(),
        "each member of a union throws set must be allowed: {diags:?}"
    );
}

#[test]
fn missing_throws_clause_is_unconstrained() {
    // No `throws` clause → the historical unconstrained behavior; the throw of
    // an int must not be flagged. This is what keeps the feature additive.
    let diags = throws_mismatches(
        r#"fn parse(s: string) {
  throw 42
}"#,
    );
    assert!(
        diags.is_empty(),
        "an unannotated callable must never be throws-checked: {diags:?}"
    );
}

#[test]
fn pipeline_throws_clause_is_enforced() {
    let diags = throws_mismatches(
        r#"pipeline run(task) throws string {
  throw 42
}"#,
    );
    assert_eq!(
        diags.len(),
        1,
        "a pipeline that throws outside its declared set must raise TYP-026: {diags:?}"
    );
}
