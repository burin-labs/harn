//! Capability access on a `Harness`-annotated receiver is checked against
//! the builtin manifest, not deferred to the VM.
//!
//! The runtime already rejects an unknown capability as a type error. Before
//! this check existed, `harn check` accepted it and the failure surfaced only
//! when the expression executed — which, for a branch that only runs during a
//! live release, meant tens of minutes into a release that then had to be
//! redone.

use super::*;

fn has(msgs: &[String], needle: &str) -> bool {
    msgs.iter().any(|m| m.contains(needle))
}

/// Help text for the first error, which is where the candidate list lives.
fn error_help(source: &str) -> String {
    check_source(source)
        .into_iter()
        .find(|d| d.severity == DiagnosticSeverity::Error)
        .and_then(|d| d.help)
        .unwrap_or_default()
}

// --- unknown capability -------------------------------------------------

#[test]
fn unknown_capability_property_is_error() {
    let errs = errors("fn main(harness: Harness) {\n  const x = harness.bogus\n  log(x)\n}");
    assert!(
        has(&errs, "`Harness` has no capability `bogus`"),
        "got: {errs:?}"
    );
}

#[test]
fn unknown_capability_method_call_is_error() {
    let errs = errors("fn main(harness: Harness) {\n  log(harness.bogus.read())\n}");
    assert!(
        has(&errs, "`Harness` has no capability `bogus`"),
        "got: {errs:?}"
    );
}

/// The exact expression that cost two release attempts: `crypto` is a pure
/// global (`sha256`), never a capability handle.
#[test]
fn harness_crypto_is_rejected_with_the_capability_list() {
    let errs = errors("fn main(harness: Harness) {\n  log(harness.crypto.sha256(\"abc\"))\n}");
    assert!(
        has(&errs, "`Harness` has no capability `crypto`"),
        "got: {errs:?}"
    );
    let help = error_help("fn main(harness: Harness) {\n  log(harness.crypto.sha256(\"abc\"))\n}");
    assert!(
        help.starts_with("available capabilities:") && help.contains("fs"),
        "expected the capability list in the help, got: {help:?}"
    );
}

#[test]
fn near_miss_capability_suggests_the_real_one() {
    let errs = errors("fn main(harness: Harness) {\n  log(harness.stdi.println(\"x\"))\n}");
    assert!(has(&errs, "did you mean `stdio`?"), "got: {errs:?}");
}

#[test]
fn optional_capability_access_is_allowed() {
    let errs = errors("fn main(harness: Harness) {\n  const x = harness?.bogus\n  log(x)\n}");
    assert!(
        !has(&errs, "has no capability"),
        "`?.` opts into the nil result the VM returns, got: {errs:?}"
    );
}

// --- valid capability use stays clean -----------------------------------

#[test]
fn valid_capability_access_has_no_errors() {
    let errs = errors(
        "fn main(harness: Harness) {\n  \
         const body = harness.fs.read_text(\"README.md\")\n  \
         harness.stdio.println(body)\n  \
         harness.stdio.println(sha256(body))\n}",
    );
    assert!(errs.is_empty(), "got: {errs:?}");
}

/// Every capability the manifest declares must survive the property check,
/// so a newly registered capability cannot be rejected by a stale list.
#[test]
fn every_declared_capability_is_accepted() {
    for capability in harn_builtin_meta::CapabilityId::ALL {
        let field = capability.field_name();
        let src =
            format!("fn main(harness: Harness) {{\n  const c = harness.{field}\n  log(c)\n}}");
        let errs = errors(&src);
        assert!(
            !has(&errs, "has no capability"),
            "capability `{field}` was rejected: {errs:?}"
        );
    }
}
