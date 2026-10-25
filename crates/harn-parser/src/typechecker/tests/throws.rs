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

#[test]
fn thrown_record_tags_select_the_error_union_arm() {
    let declarations = r#"
type CallbackFailure = {
  kind: "callback_failed",
  category: "internal",
  message: string,
  cause: {terminal_error: unknown, callback_error: unknown},
}
type CancelledFailure = {category: "cancelled", message: string}
type Failure = string | CallbackFailure | CancelledFailure
"#;
    for (record, rejected) in [
        (
            r#"{kind: "callback_failed", category: "internal", message: "failed", cause: {terminal_error: "first", callback_error: "second"}}"#,
            false,
        ),
        (r#"{category: "cancelled", message: "stopped"}"#, false),
        (
            r#"{knid: "callback_failed", category: "internal", message: "failed", cause: {terminal_error: "first", callback_error: "second"}}"#,
            true,
        ),
        (
            r#"{kind: "callback_failed", category: "internal", message: "failed", cause: {terminal_error: "first"}}"#,
            true,
        ),
        (
            r#"{...{category: "cancelled", message: "failed"}, category: "internal"}"#,
            true,
        ),
    ] {
        let source =
            format!("{declarations}\nconst result: Result<_, Failure> = try {{ throw {record} }}");
        let mismatches = check_source(&source)
            .into_iter()
            .filter(|diagnostic| diagnostic.code == Code::VariableTypeMismatch)
            .count();
        assert_eq!(mismatches, usize::from(rejected), "{record}");
    }
    let legacy = r#"const result: Result<_, string | dict> = try {
  throw {knid: "callback_failed", category: "internal", message: "failed"}
}"#;
    assert!(!check_source(legacy)
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error));
}

fn throws_mismatches(source: &str) -> Vec<String> {
    check_source(source)
        .into_iter()
        .filter(|d| d.code == Code::ThrowsTypeMismatch)
        .map(|d| d.message)
        .collect()
}

#[rstest::rstest]
#[case::throwing_the_declared_type_checks_clean(
    r#"fn parse(s: string) throws string {
  throw "bad input"
}"#,
    true
)]
#[case::union_throws_set_covers_each_member(
    r#"fn parse(s: string) throws string | int {
  if s == "" {
    throw 42
  }
  throw "bad"
}"#,
    true
)]
// No `throws` clause → the historical unconstrained behavior; the throw of
// an int must not be flagged. This is what keeps the feature additive.
#[case::missing_throws_clause_is_unconstrained(
    r"fn parse(s: string) {
  throw 42
}",
    true
)]
// --- catch-exhaustiveness: try/catch subtracts handled errors -------------
// The body throws `Parse.Bad`, which the `catch (e: Parse)` handles, so it
// does NOT count against the declared channel. The catch body then throws
// `Net.Down`, which IS declared — so the callable checks clean. If the
// caught `Parse` error leaked into the thrown set this would raise TYP-026
// (Parse is not <: Net).
#[case::caught_error_is_excluded_from_the_declared_channel(
    r"enum Parse { Bad }
enum Net { Down }
fn f() throws Net {
  try {
    throw Parse.Bad
  } catch (e: Parse) {
    throw Net.Down
  }
}",
    true
)]
// An untyped `catch` is a catch-all: every error the body throws is
// handled, so nothing escapes and the (over-broad) declared channel is not
// violated.
#[case::catch_all_absorbs_every_body_error(
    r"enum Net { Down }
fn f() throws Net {
  try {
    throw Net.Down
  } catch {
  }
}",
    true
)]
fn diagnostics_throws_mismatches(#[case] source: &str, #[case] empty: bool) {
    let diagnostics = throws_mismatches(source);
    assert_eq!(diagnostics.is_empty(), empty, "{diagnostics:?}");
}

#[test]
fn throwing_an_undeclared_type_errors() {
    let diags = throws_mismatches(
        r"fn parse(s: string) throws string {
  throw 42
}",
    );
    assert_eq!(
        diags.len(),
        1,
        "throwing a type outside the declared set must raise exactly one TYP-026: {diags:?}"
    );
}

#[test]
fn pipeline_throws_clause_is_enforced() {
    let diags = throws_mismatches(
        r"pipeline run(task) throws string {
  throw 42
}",
    );
    assert_eq!(
        diags.len(),
        1,
        "a pipeline that throws outside its declared set must raise TYP-026: {diags:?}"
    );
}

#[test]
fn uncaught_error_escapes_and_must_be_declared() {
    // The body throws `Net.Down` but the catch only handles `Parse`, so
    // `Net.Down` is rethrown and escapes. The declared channel is `Parse`,
    // which does not cover it → exactly one TYP-026.
    let diags = throws_mismatches(
        r"enum Parse { Bad }
enum Net { Down }
fn f() throws Parse {
  try {
    throw Net.Down
  } catch (e: Parse) {
  }
}",
    );
    assert_eq!(
        diags.len(),
        1,
        "an error the catch does not cover must escape and raise TYP-026: {diags:?}"
    );
}

#[test]
fn finally_body_throw_escapes() {
    // A `throw` in the `finally` block always escapes, regardless of the catch.
    // Here it is `Net.Down` against a declared `Parse` channel → TYP-026.
    let diags = throws_mismatches(
        r"enum Parse { Bad }
enum Net { Down }
fn f() throws Parse {
  try {
    throw Parse.Bad
  } catch (e: Parse) {
  } finally {
    throw Net.Down
  }
}",
    );
    assert_eq!(
        diags.len(),
        1,
        "a throw in `finally` must escape and be checked against `throws`: {diags:?}"
    );
}
