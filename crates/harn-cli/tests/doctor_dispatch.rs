#![recursion_limit = "256"]

//! Partial-port verification for `harn doctor` (harn#2312 / W12).
//!
//! The Rust shim in `crates/harn-cli/src/commands/doctor.rs` still
//! runs every probe (toolchain, providers, MCP, manifest health,
//! capability matrix, hardware snapshot, ollama, target probes) and
//! assembles a structured [`DoctorReport`]. The rendering layer
//! (human-readable section layout + JSON envelope pass-through) lives
//! in `crates/harn-stdlib/src/stdlib/cli/doctor.harn` and is
//! dispatched through the wedge so it ratchets onto the
//! self-hosted `.harn` CLI stack.
//!
//! `HARN_CLI_IMPL=rust` keeps the legacy direct-render path for the
//! parity harness (#2299) until the C1 ratchet (#2314) deletes it.
//!
//! Parity bar:
//!   * Default text: byte-for-byte identity between impls (both
//!     paths share the same `build_report`, so any per-call host
//!     variance — e.g. provider healthcheck latency — is identical
//!     because each test invocation runs the probes once and hands
//!     the result to the renderer).
//!   * JSON envelope: byte-for-byte identity between impls — the
//!     dispatch shim pre-serialises the envelope and the script
//!     echoes the bytes verbatim.

use std::collections::HashSet;
use std::process::{Command, Output};

fn harn_binary() -> &'static str {
    env!("CARGO_BIN_EXE_harn")
}

struct SubprocessOutcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

/// Spawn `harn` with a controlled environment. The fixture scrubs
/// `HARN_CLI_IMPL` / `NO_COLOR` / `HARN_COLOR` so the test owns the
/// dispatch path and terminal detection, and accepts any number of
/// per-test env overrides. Inherits the rest of the env (PATH, HOME,
/// the user's keyring backend) so the toolchain and credential probes
/// stay representative of the legacy code path.
fn run(argv: &[&str], extra_env: &[(&str, &str)]) -> SubprocessOutcome {
    let mut cmd = Command::new(harn_binary());
    for arg in argv {
        cmd.arg(arg);
    }
    for key in ["HARN_CLI_IMPL", "NO_COLOR", "HARN_COLOR"] {
        cmd.env_remove(key);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let Output {
        status,
        stdout,
        stderr,
    } = cmd.output().expect("spawn harn");
    SubprocessOutcome {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_code: status.code().unwrap_or(-1),
    }
}

fn parse_json(s: &str, label: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap_or_else(|err| {
        panic!("{label} stdout is not valid JSON: {err}\n--- payload ---\n{s}")
    })
}

/// Default `harn doctor` text output must match byte-for-byte across
/// impls. Both the dispatch path and the legacy path share the same
/// `build_report`, so any host-derived detail (`rustc` version,
/// installed targets, provider credential presence, free disk) is
/// folded into the structured report before either renderer sees it.
#[test]
fn doctor_human_text_is_byte_identical_between_impls() {
    let harn = run(&["doctor"], &[]);
    let rust = run(&["doctor"], &[("HARN_CLI_IMPL", "rust")]);
    // The exit code reflects whether the user's host has any blocking
    // checks failing today (e.g. `rustc` missing) — both impls return
    // the same code because they consume the same report.
    assert_eq!(
        harn.exit_code, rust.exit_code,
        "doctor exit code diverged: harn={} rust={}\n--- harn stderr ---\n{}\n--- rust stderr ---\n{}",
        harn.exit_code, rust.exit_code, harn.stderr, rust.stderr
    );
    assert_eq!(harn.stdout, rust.stdout, "doctor human stdout diverged");
}

/// `--json` output must also match byte-for-byte. The dispatch shim
/// pre-serialises the [`JsonEnvelope`] in Rust and hands the script
/// the canonical bytes via `HARN_DOCTOR_REPORT_ENVELOPE_JSON`; the
/// script echoes them verbatim instead of re-rendering through
/// `json_stringify_pretty` (which would alphabetise the keys).
#[test]
fn doctor_json_envelope_is_byte_identical_between_impls() {
    let harn = run(&["doctor", "--json"], &[]);
    let rust = run(&["doctor", "--json"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(
        harn.exit_code, rust.exit_code,
        "doctor --json exit code diverged: harn={} rust={}\n--- harn stderr ---\n{}\n--- rust stderr ---\n{}",
        harn.exit_code, rust.exit_code, harn.stderr, rust.stderr
    );
    assert_eq!(
        harn.stdout, rust.stdout,
        "doctor --json stdout diverged\n--- rust ---\n{}\n--- harn ---\n{}",
        rust.stdout, harn.stdout
    );
}

/// The `--json` envelope must carry the canonical doctor schema
/// version and the top-level structure agents rely on (`host`,
/// `targets`, `providers`, `checks`, `summary`, `next_step`). This
/// guards against a renderer change that silently drops a top-level
/// field — the byte-identity test would catch a re-ordering, but a
/// silent regression in the report struct itself needs an explicit
/// shape assertion.
#[test]
fn doctor_json_envelope_carries_schema_and_top_level_keys() {
    let outcome = run(&["doctor", "--json"], &[]);
    let value = parse_json(&outcome.stdout, "doctor --json");
    assert_eq!(
        value["schemaVersion"], 2,
        "doctor schema version drifted; bump DOCTOR_SCHEMA_VERSION + downstream consumers"
    );
    assert_eq!(value["ok"], true, "doctor --json should have ok=true");
    let data = &value["data"];
    let required_keys: HashSet<&str> = [
        "host",
        "providers_config_path",
        "model_defaults",
        "targets",
        "providers",
        "capabilities",
        "checks",
        "summary",
        "hardware",
        "next_step",
    ]
    .into_iter()
    .collect();
    let actual_keys: HashSet<&str> = data
        .as_object()
        .expect("doctor data is an object")
        .keys()
        .map(String::as_str)
        .collect();
    for key in &required_keys {
        assert!(
            actual_keys.contains(key),
            "doctor data missing required key '{key}' (actual: {actual_keys:?})"
        );
    }
    // Spot-check the substructures so a future renderer change can't
    // silently flatten `host` into the top level.
    assert!(data["host"]["os"].is_string(), "host.os should be string");
    assert!(
        data["host"]["arch"].is_string(),
        "host.arch should be string"
    );
    assert!(
        data["summary"]["ok"].is_number(),
        "summary.ok should be a number"
    );
    assert!(
        data["checks"].is_array(),
        "checks should be an array of check rows"
    );
}

/// Run `harn doctor` against an empty temp dir to exercise the
/// "no manifest / no skills / no metadata" path on both impls. This
/// pushes a different mix of WARN/SKIP rows through the renderer than
/// the cwd-of-the-test invocation and proves the script handles the
/// alternate shape without diverging from the Rust legacy path.
#[test]
fn doctor_in_empty_dir_is_byte_identical_between_impls() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cwd = tmp.path().to_string_lossy().into_owned();

    let mut harn_cmd = Command::new(harn_binary());
    harn_cmd.arg("doctor").current_dir(&cwd);
    for key in ["HARN_CLI_IMPL", "NO_COLOR", "HARN_COLOR"] {
        harn_cmd.env_remove(key);
    }
    let harn_out = harn_cmd.output().expect("spawn harn");

    let mut rust_cmd = Command::new(harn_binary());
    rust_cmd
        .arg("doctor")
        .current_dir(&cwd)
        .env("HARN_CLI_IMPL", "rust");
    for key in ["NO_COLOR", "HARN_COLOR"] {
        rust_cmd.env_remove(key);
    }
    let rust_out = rust_cmd.output().expect("spawn harn");

    let harn_stdout = String::from_utf8_lossy(&harn_out.stdout).into_owned();
    let rust_stdout = String::from_utf8_lossy(&rust_out.stdout).into_owned();
    let harn_exit = harn_out.status.code().unwrap_or(-1);
    let rust_exit = rust_out.status.code().unwrap_or(-1);

    assert_eq!(
        harn_exit, rust_exit,
        "doctor exit code diverged in empty dir: harn={harn_exit} rust={rust_exit}"
    );
    assert_eq!(
        harn_stdout, rust_stdout,
        "doctor stdout diverged in empty dir"
    );
}
