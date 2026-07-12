//! HARN-TYP-026 — the `throws E` declared-exception-channel check.
//!
//! Deterministic, in-process typechecker assertions on the diagnostic CODE
//! (never prose/timing): a callable whose `throw` sites all conform to its
//! declared `throws` set checks clean; a `throw` of a type the set does not
//! cover raises TYP-026; and a callable with no `throws` clause is never
//! constrained (the annotation is additive). The final group covers
//! catch-exhaustiveness: a `try`/`catch` subtracts the errors it handles from
//! the declared channel and adds those its catch/finally bodies raise, so a
//! caught error does not count and an uncaught one does — matching the VM's
//! type-selective (enum-only) `catch` dispatch.

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
        r#"fn parse(s: string) throws string | int {
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

// --- catch-exhaustiveness: try/catch subtracts handled errors -------------

#[test]
fn caught_error_is_excluded_from_the_declared_channel() {
    // The body throws `Parse.Bad`, which the `catch (e: Parse)` handles, so it
    // does NOT count against the declared channel. The catch body then throws
    // `Net.Down`, which IS declared — so the callable checks clean. If the
    // caught `Parse` error leaked into the thrown set this would raise TYP-026
    // (Parse is not <: Net).
    let diags = throws_mismatches(
        r#"enum Parse { Bad }
enum Net { Down }
fn f() throws Net {
  try {
    throw Parse.Bad
  } catch (e: Parse) {
    throw Net.Down
  }
}"#,
    );
    assert!(
        diags.is_empty(),
        "an error handled by its catch clause must not count against `throws`: {diags:?}"
    );
}

#[test]
fn uncaught_error_escapes_and_must_be_declared() {
    // The body throws `Net.Down` but the catch only handles `Parse`, so
    // `Net.Down` is rethrown and escapes. The declared channel is `Parse`,
    // which does not cover it → exactly one TYP-026.
    let diags = throws_mismatches(
        r#"enum Parse { Bad }
enum Net { Down }
fn f() throws Parse {
  try {
    throw Net.Down
  } catch (e: Parse) {
  }
}"#,
    );
    assert_eq!(
        diags.len(),
        1,
        "an error the catch does not cover must escape and raise TYP-026: {diags:?}"
    );
}

#[test]
fn catch_all_absorbs_every_body_error() {
    // An untyped `catch` is a catch-all: every error the body throws is
    // handled, so nothing escapes and the (over-broad) declared channel is not
    // violated.
    let diags = throws_mismatches(
        r#"enum Net { Down }
fn f() throws Net {
  try {
    throw Net.Down
  } catch {
  }
}"#,
    );
    assert!(
        diags.is_empty(),
        "a catch-all must absorb every body error: {diags:?}"
    );
}

#[test]
fn finally_body_throw_escapes() {
    // A `throw` in the `finally` block always escapes, regardless of the catch.
    // Here it is `Net.Down` against a declared `Parse` channel → TYP-026.
    let diags = throws_mismatches(
        r#"enum Parse { Bad }
enum Net { Down }
fn f() throws Parse {
  try {
    throw Parse.Bad
  } catch (e: Parse) {
  } finally {
    throw Net.Down
  }
}"#,
    );
    assert_eq!(
        diags.len(),
        1,
        "a throw in `finally` must escape and be checked against `throws`: {diags:?}"
    );
}
