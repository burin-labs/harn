//! `harn explain` dispatch contract tests.
//!
//! Single-code explanation rendering lives in the self-hosted CLI script.
//! `--catalog` and `--invariant` remain host-side because they are
//! diagnostics codegen surfaces.

use std::process::Command;

#[test]
fn single_code_human_text_renders_code_summary_and_body() {
    let harn = run_explain(&["HARN-TYP-014"], &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    assert!(
        harn.stdout.starts_with("HARN-TYP-014 — "),
        "stdout should begin with the code+summary header, got: {}",
        harn.stdout
    );
    assert!(
        harn.stdout.contains("## What it means"),
        "stdout should include explanation body, got: {}",
        harn.stdout
    );
    assert!(
        harn.stdout.contains("See also:"),
        "stdout should include related diagnostics, got: {}",
        harn.stdout
    );
}

#[test]
fn single_code_with_repair_includes_repair_line() {
    // HARN-OWN-001 (immutable assignment) is one of the codes that maps
    // to a repair template. The renderer must surface the same repair
    // header.
    let harn = run_explain(&["HARN-OWN-001"], &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    assert!(
        harn.stdout.contains("Repair: bindings/make-mutable"),
        "expected repair line in stdout, got: {}",
        harn.stdout
    );
}

#[test]
fn single_code_json_renders_canonical_envelope() {
    let harn = run_explain(&["HARN-TYP-014", "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    let harn_value: serde_json::Value =
        serde_json::from_str(&harn.stdout).expect("harn JSON parses");
    assert_eq!(harn_value["schemaVersion"], 1);
    assert_eq!(harn_value["code"], "HARN-TYP-014");
    assert_eq!(harn_value["category"], "TYP");
    assert_eq!(harn_value["apiStability"], "stable");
    assert!(harn_value["summary"].is_string());
    assert!(harn_value["body"].is_string());
    let related: Vec<&str> = harn_value["related"]
        .as_array()
        .expect("related is an array")
        .iter()
        .map(|v| v.as_str().expect("related entries are code strings"))
        .collect();
    assert!(
        related.contains(&"HARN-TYP-013"),
        "related should list HARN-TYP-013, got: {related:?}"
    );
}

#[test]
fn single_code_json_surfaces_repair_when_registered() {
    // HARN-OWN-001 (immutable assignment) is mapped to
    // `bindings/make-mutable` / scope-local in the registry. The envelope
    // must surface that so IDE clients and agents can dispatch on safety
    // without parsing prose.
    let harn = run_explain(&["HARN-OWN-001", "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    let value: serde_json::Value = serde_json::from_str(&harn.stdout).expect("harn JSON parses");
    let repairs = value["repairs"]
        .as_array()
        .expect("repairs should always be an array");
    assert_eq!(repairs.len(), 1, "expected one registered repair");
    assert_eq!(repairs[0]["id"], "bindings/make-mutable");
    assert_eq!(repairs[0]["safety"], "scope-local");
}

#[test]
fn single_code_json_repairs_empty_when_no_template_registered() {
    // Parser errors don't have a registered repair shape (they're
    // diagnose-only). Envelope `repairs` should still be an empty array,
    // not absent — the schema contract is that the field exists for
    // every code.
    let harn = run_explain(&["HARN-PAR-001", "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    let value: serde_json::Value = serde_json::from_str(&harn.stdout).expect("harn JSON parses");
    assert_eq!(value["repairs"], serde_json::json!([]));
}

#[test]
fn unknown_code_exits_two() {
    let harn = run_explain(&["HARN-ZZZ-999"], &[]);
    assert_eq!(harn.exit_code, 2, "harn stderr={}", harn.stderr);
}

#[test]
fn catalog_flag_stays_on_host_codegen_path() {
    // --catalog is out of scope for the .harn renderer. It is a codegen
    // tool consumed by `make sync-diagnostics-catalog`, so the dispatch
    // shim keeps routing it to the host-side catalog renderer.
    let harn = run_explain(&["--catalog", "--format", "json"], &[]);
    assert_eq!(harn.exit_code, 0, "stderr={}", harn.stderr);
    let value: serde_json::Value = serde_json::from_str(&harn.stdout).expect("catalog JSON parses");
    assert_eq!(value["schemaVersion"], 1);
    assert!(
        value["categories"].is_array(),
        "catalog should include category rows"
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
