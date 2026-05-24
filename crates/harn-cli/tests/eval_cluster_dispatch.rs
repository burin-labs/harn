#![recursion_limit = "256"]

//! Partial-port verification for `harn eval context` / `eval tool-calls`
//! / `eval model-selector` (harn#2306 / W6).
//!
//! The rendering layer for each command ships in
//! `crates/harn-stdlib/src/stdlib/cli/eval/*.harn`. This test asserts
//! parity against the legacy Rust render path on every output the wedge
//! actually owns:
//!
//!   * `eval context` — markdown body of `summary.md` (byte-identical)
//!     plus the one-line stdout summary and the `--json` pretty form
//!     (structural, since Harn's `json_stringify_pretty` sorts keys).
//!   * `eval tool-calls regression-check` — both the success stdout
//!     line and the over-budget stderr failure line (byte-identical),
//!     plus the total-cases-mismatch error path.
//!   * `eval/model_selector` — the helper script's resolution branches
//!     (kv form, colon form, alias dict, unknown alias fallback).
//!
//! Aggregation (manifest load, evaluate, scoring, llm-call fanout)
//! stays in Rust on both impls — only the formatting differs — so the
//! parity bar is byte-identity for text/markdown surfaces and
//! structural equality for JSON.
//!
//! `HARN_CLI_IMPL=rust` keeps the legacy direct-render path so this
//! test can compare both sides at runtime until the C1 ratchet (#2314)
//! deletes it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use harn_cli::dispatch::run_embedded_script;

// ─── eval context ────────────────────────────────────────────────────────

#[test]
fn eval_context_summary_md_is_byte_identical_between_impls() {
    let manifest = workspace_root().join("examples/evals/context-engineering-smoke.json");
    let harn_dir = tempfile::tempdir().expect("tempdir");
    let rust_dir = tempfile::tempdir().expect("tempdir");

    let harn = run_eval_context(&manifest, harn_dir.path(), false, &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run_eval_context(
        &manifest,
        rust_dir.path(),
        false,
        &[("HARN_CLI_IMPL", "rust")],
    );
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);

    let harn_md = fs::read_to_string(harn_dir.path().join("summary.md")).expect("harn summary.md");
    let rust_md = fs::read_to_string(rust_dir.path().join("summary.md")).expect("rust summary.md");
    assert_eq!(
        harn_md, rust_md,
        "summary.md diverged\n--- rust ---\n{}\n--- harn ---\n{}",
        rust_md, harn_md
    );
}

#[test]
fn eval_context_stdout_summary_line_is_byte_identical_between_impls() {
    let manifest = workspace_root().join("examples/evals/context-engineering-smoke.json");
    let harn_dir = tempfile::tempdir().expect("tempdir");
    let rust_dir = tempfile::tempdir().expect("tempdir");

    let harn = run_eval_context(&manifest, harn_dir.path(), false, &[]);
    let rust = run_eval_context(
        &manifest,
        rust_dir.path(),
        false,
        &[("HARN_CLI_IMPL", "rust")],
    );
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert_eq!(
        harn.stdout, rust.stdout,
        "stdout summary line diverged\n--- rust ---\n{}\n--- harn ---\n{}",
        rust.stdout, harn.stdout
    );
}

#[test]
fn eval_context_json_stdout_is_structurally_identical_between_impls() {
    // Harn's `json_stringify_pretty` sorts dict keys alphabetically;
    // serde's `to_string_pretty` emits struct fields in declaration
    // order. The wire byte order can therefore differ even though the
    // parsed shapes match — assert structural equality, not byte
    // identity, for the JSON path.
    let manifest = workspace_root().join("examples/evals/context-engineering-smoke.json");
    let harn_dir = tempfile::tempdir().expect("tempdir");
    let rust_dir = tempfile::tempdir().expect("tempdir");

    let harn = run_eval_context(&manifest, harn_dir.path(), true, &[]);
    let rust = run_eval_context(
        &manifest,
        rust_dir.path(),
        true,
        &[("HARN_CLI_IMPL", "rust")],
    );
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    let harn_value: serde_json::Value =
        serde_json::from_str(&harn.stdout).expect("harn --json stdout parses");
    let rust_value: serde_json::Value =
        serde_json::from_str(&rust.stdout).expect("rust --json stdout parses");
    assert_eq!(
        rust_value, harn_value,
        "--json stdout diverged structurally"
    );
}

#[test]
fn eval_context_summary_json_artifact_stays_byte_identical_across_impls() {
    // The on-disk `summary.json` is consumed by regression-check and
    // hosted ingestion; both paths depend on serde's struct-field order,
    // so the artifact must stay byte-identical with the legacy renderer.
    // This guards against an accidental future port that routes the JSON
    // artifact through Harn's alphabetical-key serialiser.
    let manifest = workspace_root().join("examples/evals/context-engineering-smoke.json");
    let harn_dir = tempfile::tempdir().expect("tempdir");
    let rust_dir = tempfile::tempdir().expect("tempdir");

    run_eval_context(&manifest, harn_dir.path(), false, &[]);
    run_eval_context(
        &manifest,
        rust_dir.path(),
        false,
        &[("HARN_CLI_IMPL", "rust")],
    );

    let harn_json =
        fs::read_to_string(harn_dir.path().join("summary.json")).expect("harn summary.json");
    let rust_json =
        fs::read_to_string(rust_dir.path().join("summary.json")).expect("rust summary.json");
    assert_eq!(harn_json, rust_json, "summary.json byte-diverged");
}

// ─── eval tool-calls regression-check ────────────────────────────────────

#[test]
fn tool_calls_regression_success_line_is_byte_identical_between_impls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let baseline = dir.path().join("baseline.json");
    let current = dir.path().join("current.json");
    fs::write(
        &baseline,
        r#"{"pass_rate": 0.85, "total_cases": 20, "planner": {"selector": "mock:mock", "provider": "mock", "model": "mock"}}"#,
    )
    .unwrap();
    fs::write(
        &current,
        r#"{"pass_rate": 0.84, "total_cases": 20, "planner": {"selector": "mock:mock", "provider": "mock", "model": "mock"}}"#,
    )
    .unwrap();

    let harn = run_tool_calls_regression(&current, &baseline, &[]);
    let rust = run_tool_calls_regression(&current, &baseline, &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert_eq!(
        harn.stdout, rust.stdout,
        "regression success stdout diverged"
    );
}

#[test]
fn tool_calls_regression_over_budget_failure_is_byte_identical_between_impls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let baseline = dir.path().join("baseline.json");
    let current = dir.path().join("current.json");
    fs::write(
        &baseline,
        r#"{"pass_rate": 0.85, "total_cases": 20, "planner": {"selector": "mock:mock", "provider": "mock", "model": "mock"}}"#,
    )
    .unwrap();
    fs::write(
        &current,
        r#"{"pass_rate": 0.50, "total_cases": 20, "planner": {"selector": "mock:mock", "provider": "mock", "model": "mock"}}"#,
    )
    .unwrap();

    let harn = run_tool_calls_regression(&current, &baseline, &[]);
    let rust = run_tool_calls_regression(&current, &baseline, &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(harn.exit_code, 1, "expected over-budget exit 1 on harn");
    assert_eq!(rust.exit_code, 1, "expected over-budget exit 1 on rust");
    assert_eq!(
        harn.stderr, rust.stderr,
        "regression failure stderr diverged"
    );
    // The over-budget failure path emits nothing on stdout on either
    // impl — pin that too so a future contributor doesn't accidentally
    // route the failure line through stdout under one impl.
    assert!(harn.stdout.is_empty(), "harn stdout was {}", harn.stdout);
    assert!(rust.stdout.is_empty(), "rust stdout was {}", rust.stdout);
}

#[test]
fn tool_calls_regression_total_cases_mismatch_is_byte_identical_between_impls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let baseline = dir.path().join("baseline.json");
    let current = dir.path().join("current.json");
    fs::write(
        &baseline,
        r#"{"pass_rate": 0.85, "total_cases": 20, "planner": {"selector": "mock:mock", "provider": "mock", "model": "mock"}}"#,
    )
    .unwrap();
    fs::write(
        &current,
        r#"{"pass_rate": 0.85, "total_cases": 15, "planner": {"selector": "mock:mock", "provider": "mock", "model": "mock"}}"#,
    )
    .unwrap();

    let harn = run_tool_calls_regression(&current, &baseline, &[]);
    let rust = run_tool_calls_regression(&current, &baseline, &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(harn.exit_code, 1, "expected mismatch exit 1 on harn");
    assert_eq!(rust.exit_code, 1, "expected mismatch exit 1 on rust");
    assert_eq!(
        harn.stderr, rust.stderr,
        "total-cases-mismatch stderr diverged"
    );
}

// ─── eval/model_selector helper script ───────────────────────────────────
//
// The model_selector dispatch surface is helper-only — no Rust CLI
// subcommand actually calls into it today (sibling .harn scripts and
// the Rust-side `eval_model_selector::resolve_selector` are the
// production consumers). Exercising it directly through the wedge
// black-boxes the resolution branches and pins them against the
// expected behaviors.

#[tokio::test]
async fn model_selector_resolves_provider_model_kv_form() {
    let outcome = dispatch_model_selector("provider=openrouter,model=google/gemma", "{}").await;
    assert_eq!(outcome.exit_code, 0, "stderr={}", outcome.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(outcome.stdout.trim()).expect("stdout parses");
    assert_eq!(parsed["provider"], serde_json::json!("openrouter"));
    assert_eq!(parsed["model"], serde_json::json!("google/gemma"));
    assert_eq!(
        parsed["selector"],
        serde_json::json!("provider=openrouter,model=google/gemma")
    );
}

#[tokio::test]
async fn model_selector_resolves_colon_form() {
    let outcome = dispatch_model_selector("ollama:qwen3.5", "{}").await;
    assert_eq!(outcome.exit_code, 0, "stderr={}", outcome.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(outcome.stdout.trim()).expect("stdout parses");
    assert_eq!(parsed["provider"], serde_json::json!("ollama"));
    assert_eq!(parsed["model"], serde_json::json!("qwen3.5"));
}

#[tokio::test]
async fn model_selector_resolves_alias_via_provided_dict() {
    let aliases =
        r#"{"claude-opus-4-7":{"provider":"anthropic","model":"claude-opus-4-7-20260101"}}"#;
    let outcome = dispatch_model_selector("claude-opus-4-7", aliases).await;
    assert_eq!(outcome.exit_code, 0, "stderr={}", outcome.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(outcome.stdout.trim()).expect("stdout parses");
    assert_eq!(parsed["provider"], serde_json::json!("anthropic"));
    assert_eq!(
        parsed["model"],
        serde_json::json!("claude-opus-4-7-20260101")
    );
}

#[tokio::test]
async fn model_selector_falls_back_to_input_when_alias_missing() {
    // Mirrors the legacy Rust `resolve_selector` fallback when
    // `harn_vm::llm_config::resolve_model_info` returns the input
    // verbatim — provider and model both echo the unknown selector.
    let outcome = dispatch_model_selector("unknown-alias", "{}").await;
    assert_eq!(outcome.exit_code, 0, "stderr={}", outcome.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(outcome.stdout.trim()).expect("stdout parses");
    assert_eq!(parsed["provider"], serde_json::json!("unknown-alias"));
    assert_eq!(parsed["model"], serde_json::json!("unknown-alias"));
}

#[tokio::test]
async fn model_selector_missing_input_returns_software_error() {
    // Calling the helper without HARN_MODEL_SELECTOR_INPUT set is a
    // shim bug. The script returns EX_SOFTWARE (70) so the failure
    // surfaces clearly without crashing the host.
    //
    // Hold the dispatch lock + actively unset the env var so concurrent
    // `dispatch_model_selector` callers (which set the var under the
    // same lock) can't leak it into this test's window.
    //
    // Note: we only assert on `exit_code` here — `harness.stdio.eprintln`
    // bypasses the dispatch wedge's `outcome.stderr` buffer (writes
    // directly to real stderr when `STDERR_CAPTURING` is off, which is
    // the in-process default), so the diagnostic text is observable to
    // the user but not to `run_embedded_script`'s return value. The
    // companion text is exercised end-to-end through the regression-
    // check subprocess tests above.
    let outcome = {
        let _guard = MODEL_SELECTOR_DISPATCH_LOCK.lock().await;
        let _input = harn_cli::env_guard::ScopedEnvVar::unset("HARN_MODEL_SELECTOR_INPUT");
        run_embedded_script("eval/model_selector", vec![], false).await
    };
    assert_eq!(outcome.exit_code, 70);
}

// ─── helpers ─────────────────────────────────────────────────────────────

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives under crates/harn-cli")
}

struct SubprocessOutcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

fn run_eval_context(
    manifest: &Path,
    output: &Path,
    json: bool,
    extra_env: &[(&str, &str)],
) -> SubprocessOutcome {
    let mut argv = vec![
        "eval".to_string(),
        "context".to_string(),
        manifest.display().to_string(),
        "--output".to_string(),
        output.display().to_string(),
    ];
    if json {
        argv.push("--json".to_string());
    }
    run_harn(&argv, extra_env)
}

fn run_tool_calls_regression(
    current: &PathBuf,
    against: &PathBuf,
    extra_env: &[(&str, &str)],
) -> SubprocessOutcome {
    run_harn(
        &[
            "eval".to_string(),
            "tool-calls".to_string(),
            "regression-check".to_string(),
            "--current".to_string(),
            current.display().to_string(),
            "--against".to_string(),
            against.display().to_string(),
        ],
        extra_env,
    )
}

fn run_harn(argv: &[String], extra_env: &[(&str, &str)]) -> SubprocessOutcome {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harn"));
    for arg in argv {
        cmd.arg(arg);
    }
    // Drop ambient env that could perturb terminal renders across the
    // two subprocess invocations.
    for key in ["NO_COLOR", "HARN_COLOR", "HARN_CLI_IMPL"] {
        cmd.env_remove(key);
    }
    let mut env_map: BTreeMap<&str, &str> = BTreeMap::new();
    for (k, v) in extra_env {
        env_map.insert(*k, *v);
    }
    for (k, v) in &env_map {
        cmd.env(*k, *v);
    }
    let output = cmd.output().expect("spawn harn");
    SubprocessOutcome {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    }
}

struct DispatchOutcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

/// Shared dispatch lock for the in-process `eval/model_selector` tests
/// so concurrent tokio test runners don't race on the global env vars
/// the script reads. Mirrors the production dispatch shims in
/// `eval_context.rs` / `eval_tool_calls.rs`.
static MODEL_SELECTOR_DISPATCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn dispatch_model_selector(input: &str, aliases_json: &str) -> DispatchOutcome {
    let _guard = MODEL_SELECTOR_DISPATCH_LOCK.lock().await;
    let _input = harn_cli::env_guard::ScopedEnvVar::set("HARN_MODEL_SELECTOR_INPUT", input);
    let _aliases =
        harn_cli::env_guard::ScopedEnvVar::set("HARN_MODEL_SELECTOR_ALIASES_JSON", aliases_json);
    let outcome = run_embedded_script("eval/model_selector", vec![], false).await;
    DispatchOutcome {
        stdout: outcome.stdout,
        stderr: outcome.stderr,
        exit_code: outcome.exit_code,
    }
}
