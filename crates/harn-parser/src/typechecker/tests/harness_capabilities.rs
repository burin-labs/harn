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

#[rstest::rstest]
#[case::unknown_capability_property_is_error(
    "fn main(harness: Harness) {\n  const x = harness.bogus\n  log(x)\n}",
    "`Harness` has no capability `bogus`"
)]
#[case::unknown_capability_method_call_is_error(
    "fn main(harness: Harness) {\n  log(harness.bogus.read())\n}",
    "`Harness` has no capability `bogus`"
)]
#[case::near_miss_capability_suggests_the_real_one(
    "fn main(harness: Harness) {\n  log(harness.stdi.println(\"x\"))\n}",
    "did you mean `stdio`?"
)]
// Method declarations must be available to the parser without starting a VM.
#[case::unknown_method_on_a_capability_is_error(
    "fn main(harness: Harness) {\n  log(harness.fs.bogus_method())\n}",
    "value of type HarnessFs has no method `bogus_method`"
)]
#[case::a_near_miss_method_suggests_the_real_name(
    "fn main(harness: Harness) {\n  log(harness.fs.read_txt(\"a\"))\n}",
    "did you mean `read_text`?"
)]
// Narrow handles reach the same check through their own type names.
#[case::unknown_method_on_a_narrow_handle_is_error(
    "fn read(fs: HarnessFs) {\n  log(fs.bogus_method())\n}",
    "value of type HarnessFs has no method `bogus_method`"
)]
fn invalid_capability_uses_are_rejected(#[case] case_input: &str, #[case] case_expected: &str) {
    let errs = errors(case_input);
    assert!(has(&errs, case_expected), "got: {errs:?}");
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

#[test]
fn secret_mutation_options_and_receipts_match_the_runtime() {
    let source = r#"
fn write_secret(secrets: HarnessSecrets) -> dict {
  return secrets.write("token", "v1", nil, 5000)
}
fn write_timed_secret(secrets: HarnessSecrets) -> dict {
  return secrets.write("token", "v1", nil, 5s)
}
fn rotate_secret(secrets: HarnessSecrets) -> dict {
  return secrets.rotate("token", "v2", nil, {grace_ms: 250, ttl_ms: 7500})
}
"#;
    let diagnostics = check_source(source);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    for call in [
        "secrets.write(\"token\", \"v1\", nil, 5000, 1)",
        "secrets.rotate(\"token\", \"v2\", nil, {}, 1)",
    ] {
        let source = format!("fn update(secrets: HarnessSecrets) -> dict {{ return {call} }}");
        let diagnostics = check_source(&source);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::BuiltinArity),
            "an unsupported fifth argument must still be rejected: {diagnostics:?}"
        );
    }
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

// --- unknown method on a known capability --------------------------------

/// The falsifier for the whole change. `harness.runtime.shared_cell` is
/// declared by `#[harn_builtin]` inside `harn-vm`, so it is absent from the
/// installed manifest in a parser-only process. Reporting the miss without
/// consulting the generated projection regressed 41 `harn-vm` tests with
/// exactly this shape.
#[test]
fn a_vm_declared_method_is_accepted_without_the_vm() {
    let errs =
        errors("fn main(harness: Harness) {\n  log(harness.runtime.shared_cell(\"k\", 1))\n}");
    assert!(
        !has(&errs, "has no method"),
        "a VM-declared method must not be reported as unknown: {errs:?}"
    );
}

/// An embedder's own host operation is declared, not contracted.
///
/// The host registers it at runtime, so no static contract in this workspace
/// owns it and the method check would otherwise report correct code as
/// undeclared. Both assertions live in one test because the declaration is
/// process-global: split across two tests, the "before" case would depend on
/// which test ran first. The operation name is unique to this test so the
/// installed entry cannot mask a real miss elsewhere.
#[test]
fn declared_host_operation_is_not_reported_as_unknown() {
    let source =
        "fn main(harness: Harness) {\n  log(harness.runtime.embedder_probe_operation())\n}";

    let before = errors(source);
    assert!(
        has(&before, "has no method `embedder_probe_operation`"),
        "an undeclared host operation must still be reported: {before:?}"
    );

    crate::install_declared_host_operations([("runtime", "embedder_probe_operation")]);

    let after = errors(source);
    assert!(
        !has(&after, "embedder_probe_operation"),
        "a declared host operation must be accepted: {after:?}"
    );
}
