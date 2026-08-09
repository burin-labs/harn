//! A repo may declare that a fixture is supposed to be unparseable (harn#6264).
//!
//! `harn fix --apply .` walks every `.harn` under its target. Conformance
//! suites keep fixtures whose whole purpose is to be rejected by the parser, so
//! a repo-wide codemod always meets them. Treating that as a run failure made
//! every consuming repo that tests its own parser errors permanently
//! un-bumpable — the reusable bump workflow runs the codemod repo-wide under
//! `set -euo pipefail`, and burin-code sat nine patch releases behind on
//! exactly this.
//!
//! The declaration already existed: the expected diagnostic lives in a sibling
//! `.error` file, a convention this repo and burin-code both use.

use super::*;

/// A fixture that cannot parse, plus a healthy file so the run has real work.
const BROKEN: &str = "fn unterminated(harness: Harness) {\n";
const HEALTHY: &str = "fn uses_net(harness: Harness) {\n  harness.net.get(\"https://x\")\n}\n";

fn apply_dir(files: &[(&str, &str)]) -> Result<ApplyResult, String> {
    let temp = tempfile::TempDir::new().unwrap();
    for (name, body) in files {
        fs::write(temp.path().join(name), body).unwrap();
    }
    apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
}

#[test]
fn an_undeclared_parse_failure_still_fails_the_run() {
    // The falsifier for the test below. If this ever passes, the fix has
    // stopped distinguishing a declared fixture from a corrupt file, and the
    // whole category has been silently suppressed instead.
    let result = apply_dir(&[("broken.harn", BROKEN), ("healthy.harn", HEALTHY)]);
    let result = result.expect("apply itself should succeed; the caller decides fatality");
    assert_eq!(
        result.skipped_files.len(),
        1,
        "an undeclared parse failure must remain a skipped file: {result:#?}"
    );
    assert!(
        result.declared_invalid_files.is_empty(),
        "nothing declared it: {result:#?}"
    );
}

#[test]
fn a_sibling_error_file_declares_the_fixture_expected_invalid() {
    let result = apply_dir(&[
        ("broken.harn", BROKEN),
        ("broken.error", "unexpected end of file, expected }\n"),
        ("healthy.harn", HEALTHY),
    ])
    .expect("apply should succeed");
    assert!(
        result.skipped_files.is_empty(),
        "a declared fixture must not land in skipped_files, which is what fails \
         the run: {result:#?}"
    );
    assert_eq!(
        result.declared_invalid_files.len(),
        1,
        "it should still be reported, not silently dropped: {result:#?}"
    );
    assert!(
        result.declared_invalid_files[0]
            .path
            .ends_with("broken.harn"),
        "{result:#?}"
    );
}

#[test]
fn a_declared_fixture_is_still_never_rewritten() {
    // Not failing the run must not be confused with becoming repairable: the
    // file did not parse, so it has no spans to edit. Assert the bytes.
    let temp = tempfile::TempDir::new().unwrap();
    let broken = temp.path().join("broken.harn");
    fs::write(&broken, BROKEN).unwrap();
    fs::write(temp.path().join("broken.error"), "expected }\n").unwrap();
    fs::write(temp.path().join("healthy.harn"), HEALTHY).unwrap();

    apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        FixOptions::capability_migrations(),
    )
    .expect("apply should succeed");

    assert_eq!(
        fs::read_to_string(&broken).unwrap(),
        BROKEN,
        "the declared fixture must be byte-identical"
    );
}
