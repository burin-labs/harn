//! HARN-NAM-101 — the `fn main(harness: Harness)` entrypoint check.
//!
//! Exercise both the happy path and the four classes of bad signature so
//! regressions in `check_main_signature` surface as test failures rather
//! than as conformance-suite drift.

use super::*;
use crate::diagnostic_codes::Code;

fn nam_101(source: &str) -> Vec<String> {
    check_source(source)
        .into_iter()
        .filter(|d| d.code == Code::InvalidMainSignature)
        .map(|d| d.message)
        .collect()
}

#[test]
fn accepts_canonical_signature() {
    let diags = nam_101(
        r#"fn main(harness: Harness) {
  harness.stdio.println("hi")
}"#,
    );
    assert!(
        diags.is_empty(),
        "canonical `main(harness: Harness)` must not raise NAM-101: {diags:?}"
    );
}

#[test]
fn accepts_underscore_harness_opt_out() {
    let diags = nam_101("fn main(_harness: Harness) {}");
    assert!(
        diags.is_empty(),
        "`_harness` is the unused-capability opt-out and must not raise NAM-101: {diags:?}"
    );
}

#[test]
fn rejects_zero_arg_main() {
    let diags = nam_101("fn main() {}");
    assert_eq!(diags.len(), 1);
    assert!(diags[0].contains("single `harness: Harness` parameter"));
}

#[test]
fn rejects_wrong_param_name() {
    let diags = nam_101("fn main(ctx: Harness) {}");
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0].contains("expected `harness` or `_harness`"),
        "expected wrong-name diagnostic, got: {diags:?}"
    );
}

#[test]
fn rejects_wrong_param_type() {
    let diags = nam_101("fn main(harness: string) {}");
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0].contains("type must be `Harness`"),
        "expected wrong-type diagnostic, got: {diags:?}"
    );
}

#[test]
fn rejects_missing_type_annotation() {
    let diags = nam_101("fn main(harness) {}");
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0].contains("explicit `Harness` type annotation"),
        "expected missing-annotation diagnostic, got: {diags:?}"
    );
}

#[test]
fn rejects_extra_params() {
    let diags = nam_101("fn main(harness: Harness, argv: list) {}");
    assert_eq!(diags.len(), 1);
    assert!(
        diags[0].contains("exactly one parameter"),
        "expected too-many-params diagnostic, got: {diags:?}"
    );
}

#[test]
fn rejects_main_with_default_value() {
    // A default value flips the param into a non-canonical shape: the
    // runtime always supplies `harness` so `harness: Harness = …` would
    // never use its default. The check enforces the canonical shape.
    let diags = nam_101("fn main(harness: Harness = nil) {}");
    assert_eq!(diags.len(), 1, "default-value variant should be rejected");
}

#[test]
fn non_main_fns_are_unrestricted() {
    let diags = nam_101("fn helper(x: int) {}");
    assert!(diags.is_empty(), "only `fn main` is constrained: {diags:?}");
}
