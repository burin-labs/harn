//! `harn version` dispatch contract tests.
//!
//! The command renders through the self-hosted CLI script. These tests
//! keep the human banner and JSON envelope shape pinned without
//! byte-pinning the release version string.

use std::process::Command;

use super::build_revision;

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
    assert_eq!(harn_value["schemaVersion"], 2);
    assert_eq!(harn_value["ok"], true);
    assert!(harn_value["data"]["version"].is_string());
    assert_source_revision(&harn_value);
    assert_runtime_content_fingerprint(&harn_value);
}

#[test]
fn version_json_ignores_runtime_revision_environment() {
    let baseline = run_version_subprocess(true, &[]);
    assert_eq!(baseline.exit_code, 0, "stderr={}", baseline.stderr);
    let baseline: serde_json::Value =
        serde_json::from_str(&baseline.stdout).expect("baseline version JSON");
    let harn = run_version_subprocess(
        true,
        &[
            (
                "HARN_BUILD_REVISION",
                "ffffffffffffffffffffffffffffffffffffffff",
            ),
            (
                "HARN_RUNTIME_CONTENT_FINGERPRINT",
                "{\"schema\":\"host-supplied\"}",
            ),
        ],
    );
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    let value: serde_json::Value = serde_json::from_str(&harn.stdout).expect("version JSON");
    assert_source_revision(&value);
    assert_runtime_content_fingerprint(&value);
    assert_eq!(
        value["data"]["runtime_content_fingerprint"],
        baseline["data"]["runtime_content_fingerprint"],
        "environment overrides must not change the launched artifact's identity"
    );
}

fn assert_runtime_content_fingerprint(value: &serde_json::Value) {
    let actual: harn_vm::RuntimeContentFingerprint =
        serde_json::from_value(value["data"]["runtime_content_fingerprint"].clone())
            .expect("typed runtime content fingerprint");
    let mut expected = harn_vm::runtime_content_fingerprint().clone();
    assert!(
        actual.artifact_id.as_ref().is_some_and(|id| !id.is_empty()),
        "the CLI must measure its native artifact"
    );
    assert_eq!(actual.content_sha256.len(), 64);
    assert!(actual
        .content_sha256
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit()));
    // The CLI and this test executable share VM inputs but have different native images.
    assert_ne!(actual.artifact_id, expected.artifact_id);
    assert_ne!(actual.content_sha256, expected.content_sha256);
    expected.artifact_id = actual.artifact_id.clone();
    expected.content_sha256 = actual.content_sha256.clone();
    expected.source_revision = serde_json::from_value(value["data"]["source_revision"].clone())
        .expect("typed source revision");
    assert_eq!(actual, expected);
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
    let mut cmd = Command::new(crate::test_util::cli::binary_path());
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
