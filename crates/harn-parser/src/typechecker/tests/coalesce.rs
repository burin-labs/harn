//! Type inference for the nil-coalesce operator `??`.
//!
//! The load-bearing contract: `a ?? b` is `non_nil(a) | b`. The subtle case
//! is an `a` whose type the checker cannot name (opaque `dict` reads, untyped
//! params) — there `non_nil(a)` is unknown too, and unknown absorbs `b`, so the
//! whole expression stays unknown rather than collapsing to the fallback's
//! type. Collapsing to the fallback (the old `(None, _) => right` rule) made
//! `opaque_dict?.field ?? nil` infer as `nil`, which then refused the sound
//! `type_of(x) == "T"` narrowing a caller relies on. See harn#4547.

use super::*;

/// `opaque?.field ?? nil` stays unknown, so a `type_of` guard can narrow it
/// and return the tested type. Regression anchor for the burin repin breakage
/// (`legacy_timing` in scripts/lib/eval-speed-recon.harn).
#[test]
fn test_dict_optional_read_coalesce_nil_narrows() {
    let errs = errors(
        r#"fn f(analysis: dict) -> dict {
  const timing = analysis?.timing ?? nil
  if type_of(timing) == "dict" { return timing }
  return {}
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

/// The subscript form takes a different inference path but must behave the same.
#[test]
fn test_dict_optional_subscript_coalesce_nil_narrows() {
    let errs = errors(
        r#"fn f(analysis: dict) -> dict {
  const timing = analysis?["timing"] ?? nil
  if type_of(timing) == "dict" { return timing }
  return {}
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

/// Unknown left with a non-nil fallback and an `int` guard narrows soundly.
#[test]
fn test_dict_read_coalesce_nil_int_guard() {
    let errs = errors(
        r#"fn f(analysis: dict) -> int {
  const n = analysis?.count ?? nil
  if type_of(n) == "int" { return n }
  return 0
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

/// A dict fallback keeps the value dict-shaped for the guard.
#[test]
fn test_dict_read_coalesce_dict_fallback() {
    let errs = errors(
        r#"fn f(analysis: dict) -> dict {
  const t = analysis?.timing ?? {}
  if type_of(t) == "dict" { return t }
  return {}
}"#,
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

/// A genuinely nilable left coalesced with a concrete default drops nil.
#[test]
fn test_nilable_coalesce_concrete_default_drops_nil() {
    let errs = errors(r#"fn f(x: string?) -> string { return x ?? "d" }"#);
    assert!(errs.is_empty(), "got: {errs:?}");
}

/// `??` never *launders* nilability: `x ?? nil` on a nilable stays nilable, so
/// returning it unguarded is still rejected. This is the soundness floor the
/// unknown-left fix must not erode.
#[test]
fn test_nilable_coalesce_nil_stays_nilable() {
    let errs = errors(
        r"fn f(x: string?) -> string {
  const y = x ?? nil
  return y
}",
    );
    assert!(
        errs.iter().any(|e| e.contains("string?")),
        "expected a nilable-return rejection, got: {errs:?}"
    );
}

/// Returning a nilable with no guard and no coalesce is still rejected — the
/// unknown-left change does not touch typed nilable flows.
#[test]
fn test_unguarded_nilable_return_still_rejected() {
    let errs = errors(r"fn f(x: string?) -> string { return x }");
    assert!(
        errs.iter().any(|e| e.contains("string?")),
        "expected a nilable-return rejection, got: {errs:?}"
    );
}
