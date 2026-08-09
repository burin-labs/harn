//! `harn version` dispatch contract tests.
//!
//! The command renders through the self-hosted CLI script. These tests
//! keep the human banner and JSON envelope shape pinned without
//! byte-pinning the release version string.

use std::process::Command;

#[path = "../../build_support/build_revision.rs"]
#[allow(dead_code)]
mod build_revision;

#[test]
fn version_dispatch_renders_banner_with_version() {
    let outcome = run_version_subprocess(false, &[]);
    assert_eq!(outcome.exit_code, 0, "stderr={}", outcome.stderr);
    // Banner ends with two newlines (raw string newline + println newline).
    assert!(outcome.stdout.ends_with("\n\n"), "banner trailing newlines");
    assert!(
        outcome.stdout.contains("the agent harness language"),
        "banner tagline missing; stdout={}",
        outcome.stdout
    );
    assert!(
        outcome.stdout.contains("harn v"),
        "banner version prefix missing; stdout={}",
        outcome.stdout
    );
}

#[test]
fn version_json_dispatch_renders_canonical_envelope() {
    let harn = run_version_subprocess(true, &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    let harn_value: serde_json::Value =
        serde_json::from_str(&harn.stdout).expect("harn JSON parses");
    assert_eq!(harn_value["schemaVersion"], 1);
    assert_eq!(harn_value["ok"], true);
    assert!(harn_value["data"]["version"].is_string());
    assert_source_revision(&harn_value);
}

#[test]
fn version_json_ignores_runtime_revision_environment() {
    let harn = run_version_subprocess(
        true,
        &[(
            "HARN_BUILD_REVISION",
            "ffffffffffffffffffffffffffffffffffffffff",
        )],
    );
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    let value: serde_json::Value = serde_json::from_str(&harn.stdout).expect("version JSON");
    assert_source_revision(&value);
}

#[test]
fn build_revision_normalization_covers_populated_and_unavailable_inputs() {
    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(
        build_revision::normalize(Some(REVISION)),
        Ok(Some(REVISION))
    );
    assert_eq!(build_revision::normalize(None), Ok(None));
    assert_eq!(build_revision::normalize(Some("  ")), Ok(None));
    assert!(build_revision::normalize(Some("short")).is_err());
    assert!(build_revision::normalize(Some("0123456789ABCDEF0123456789ABCDEF01234567")).is_err());
}

fn assert_source_revision(value: &serde_json::Value) {
    let actual = &value["data"]["source_revision"];
    let expected = env!("HARN_BUILD_REVISION");
    if expected.is_empty() {
        assert!(actual.is_null(), "source_revision={actual}");
    } else {
        assert_eq!(actual.as_str(), Some(expected));
    }
}

struct SubprocessOutcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

fn run_version_subprocess(json: bool, extra_env: &[(&str, &str)]) -> SubprocessOutcome {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harn"));
    cmd.arg("version");
    if json {
        cmd.arg("--json");
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("spawn harn version");
    SubprocessOutcome {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    }
}
