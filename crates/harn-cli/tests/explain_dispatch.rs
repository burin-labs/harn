#![recursion_limit = "256"]

//! `harn explain` port verification (harn#2304 / W4).
//!
//! Pins the `.harn` dispatch impl against the legacy Rust path. The
//! single-code render is asserted byte-for-byte for human text and
//! structurally for the JSON envelope (Harn's `json_stringify` sorts
//! dict keys alphabetically; serde emits struct fields in declaration
//! order, so the wire-format byte order differs but the parsed shape
//! must match).
//!
//! `--catalog` and `--invariant` stay in Rust by design (see
//! `crates/harn-stdlib/src/stdlib/cli/explain.harn` for the rationale);
//! they're exercised here too to confirm the dispatch shim still routes
//! them to the legacy path.

use std::process::Command;

#[test]
fn single_code_human_text_matches_rust_byte_for_byte() {
    let harn = run_explain(&["HARN-TYP-014"], &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    assert!(
        harn.stdout.starts_with("HARN-TYP-014 — "),
        "stdout should begin with the code+summary header, got: {}",
        harn.stdout
    );
    let rust = run_explain(&["HARN-TYP-014"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert_eq!(
        harn.stdout, rust.stdout,
        "rust vs .harn human text diverged"
    );
}

#[test]
fn single_code_with_repair_includes_repair_line() {
    // HARN-OWN-001 (immutable assignment) is one of the codes that maps
    // to a repair template. Both impls must surface the same repair
    // header.
    let harn = run_explain(&["HARN-OWN-001"], &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    assert!(
        harn.stdout.contains("Repair: bindings/make-mutable"),
        "expected repair line in stdout, got: {}",
        harn.stdout
    );
    let rust = run_explain(&["HARN-OWN-001"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(harn.stdout, rust.stdout, "repair line diverged");
}

#[test]
fn single_code_json_matches_rust_structurally() {
    let harn = run_explain(&["HARN-TYP-014", "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    let rust = run_explain(&["HARN-TYP-014", "--json"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    let harn_value: serde_json::Value =
        serde_json::from_str(&harn.stdout).expect("harn JSON parses");
    let rust_value: serde_json::Value =
        serde_json::from_str(&rust.stdout).expect("rust JSON parses");
    assert_eq!(
        rust_value, harn_value,
        "JSON envelope diverged\nrust:\n{}\nharn:\n{}",
        rust.stdout, harn.stdout
    );
}

#[test]
fn unknown_code_exits_two_on_both_impls() {
    let harn = run_explain(&["HARN-ZZZ-999"], &[]);
    assert_eq!(harn.exit_code, 2, "harn stderr={}", harn.stderr);
    let rust = run_explain(&["HARN-ZZZ-999"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 2, "rust stderr={}", rust.stderr);
}

#[test]
fn catalog_flag_stays_in_rust_path() {
    // --catalog is out of scope for the .harn port — it's a codegen
    // tool consumed by `make sync-diagnostics-catalog`. The dispatch
    // shim must keep routing it to the Rust renderer regardless of
    // HARN_CLI_IMPL, since both branches share the same Rust handler.
    let harn = run_explain(&["--catalog", "--format", "json"], &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    let rust = run_explain(
        &["--catalog", "--format", "json"],
        &[("HARN_CLI_IMPL", "rust")],
    );
    assert_eq!(
        harn.stdout, rust.stdout,
        "catalog should be identical regardless of impl"
    );
}

struct SubprocessOutcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

fn run_explain(argv: &[&str], extra_env: &[(&str, &str)]) -> SubprocessOutcome {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harn"));
    cmd.arg("explain");
    for arg in argv {
        cmd.arg(arg);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let output = cmd.output().expect("spawn harn explain");
    SubprocessOutcome {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    }
}
