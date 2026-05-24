#![recursion_limit = "256"]

//! `harn version` port verification (harn#2301 / W1).
//!
//! Runs the dispatched `.harn` implementation and the legacy Rust one
//! through the public dispatch helper, then asserts shape parity for
//! both human-banner and JSON-envelope outputs. We can't byte-pin the
//! version string (it bumps every release), so the test extracts the
//! payload and compares structure.
//!
//! When the parity-snapshot harness (harn#2299 / G6) graduates to
//! per-W-ticket fixtures, the byte-exact comparison will move there
//! with a record-on-bump update flow.

use std::process::Command;

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
    // The Rust-path baseline must produce identical output.
    let rust = run_version_subprocess(false, &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(
        rust.stdout, outcome.stdout,
        "rust vs .harn banner output diverged"
    );
}

#[test]
fn version_json_dispatch_matches_rust_envelope() {
    let harn = run_version_subprocess(true, &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    let rust = run_version_subprocess(true, &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    // Structural compare: byte-for-byte differs because Harn's
    // json_stringify sorts dict keys alphabetically while serde
    // emits struct fields in declaration order. Both serialize the
    // same VersionInfo envelope.
    let rust_value: serde_json::Value =
        serde_json::from_str(&rust.stdout).expect("rust JSON parses");
    let harn_value: serde_json::Value =
        serde_json::from_str(&harn.stdout).expect("harn JSON parses");
    assert_eq!(
        rust_value, harn_value,
        "rust vs .harn JSON envelope diverged\nrust:\n{}\nharn:\n{}",
        rust.stdout, harn.stdout
    );
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
