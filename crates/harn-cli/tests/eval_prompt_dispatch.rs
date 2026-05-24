#![recursion_limit = "256"]

//! `harn eval prompt` partial-port verification (harn#2305 / W5).
//!
//! Asserts that the rendering layer ported to
//! `crates/harn-stdlib/src/stdlib/cli/eval/prompt.harn` produces the
//! same output as the legacy Rust path. Aggregation (fleet rendering,
//! run/judge fanout, context-fixture evaluation) stays in Rust on both
//! impls — only the formatting differs — so byte-for-byte parity is
//! the bar for terminal/HTML, and structural-JSON parity is the bar
//! for `--output json`.
//!
//! The `HARN_CLI_IMPL=rust` escape hatch keeps the legacy direct-render
//! path so this test can compare both sides at runtime until the C1
//! ratchet (#2314) deletes it.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

/// A four-profile mock-free render fixture: each fleet member maps to a
/// distinct capability family (claude / gpt / gemini / qwen) so the
/// rendered envelopes differ across the report, and the terminal
/// renderer's "diff vs #0" summary actually fires.
const RENDER_TEMPLATE: &str = "{{ if llm.capabilities.native_tools }}\
native_tools: call finish_task() when done.\n\
{{ else }}\
text_tools: emit `<<DONE>>` when done.\n\
{{ end }}\
provider={{ llm.provider }} family={{ llm.family }}\n";

const FLEET: &[&str] = &[
    "claude-3-5-sonnet",
    "gpt-4o",
    "gemini-1.5-pro",
    "ollama:qwen3.5",
];

#[test]
fn terminal_output_is_byte_identical_between_impls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let template = dir.path().join("system.harn.prompt");
    fs::write(&template, RENDER_TEMPLATE).expect("write template");

    let harn = run_eval_prompt(&template, "terminal", &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);

    let rust = run_eval_prompt(&template, "terminal", &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);

    assert_eq!(
        harn.stdout, rust.stdout,
        "terminal stdout diverged\n--- rust ---\n{}\n--- harn ---\n{}",
        rust.stdout, harn.stdout
    );
}

#[test]
fn html_output_is_byte_identical_between_impls() {
    let dir = tempfile::tempdir().expect("tempdir");
    let template = dir.path().join("system.harn.prompt");
    fs::write(&template, RENDER_TEMPLATE).expect("write template");

    let harn = run_eval_prompt(&template, "html", &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);

    let rust = run_eval_prompt(&template, "html", &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);

    assert_eq!(
        harn.stdout, rust.stdout,
        "html stdout diverged\n--- rust ---\n{}\n--- harn ---\n{}",
        rust.stdout, harn.stdout
    );
}

#[test]
fn json_output_is_structurally_identical_between_impls() {
    // Harn's `json_stringify_pretty` sorts dict keys alphabetically;
    // serde's `to_string_pretty` emits struct fields in declaration
    // order. The wire byte order can therefore differ even though the
    // parsed shapes match — assert structural equality, not byte
    // identity, for the JSON path.
    let dir = tempfile::tempdir().expect("tempdir");
    let template = dir.path().join("system.harn.prompt");
    fs::write(&template, RENDER_TEMPLATE).expect("write template");

    let harn = run_eval_prompt(&template, "json", &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);

    let rust = run_eval_prompt(&template, "json", &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);

    let harn_value: serde_json::Value =
        serde_json::from_str(&harn.stdout).expect("harn JSON parses");
    let rust_value: serde_json::Value =
        serde_json::from_str(&rust.stdout).expect("rust JSON parses");
    assert_eq!(
        rust_value, harn_value,
        "json shape diverged\n--- rust ---\n{}\n--- harn ---\n{}",
        rust.stdout, harn.stdout
    );
}

#[test]
fn out_file_writes_match_between_impls() {
    // Exercises the script's `--out-file` (HARN_EVAL_PROMPT_OUT_FILE)
    // branch: the rendered payload goes to disk, and a "wrote <path>"
    // line goes to stderr. Both stdout and on-disk bytes must match.
    let dir = tempfile::tempdir().expect("tempdir");
    let template = dir.path().join("system.harn.prompt");
    fs::write(&template, RENDER_TEMPLATE).expect("write template");

    let harn_out = dir.path().join("harn.txt");
    let rust_out = dir.path().join("rust.txt");

    let harn = run_eval_prompt_with(
        &[
            template.to_str().unwrap(),
            "--fleet",
            &FLEET.join(","),
            "--output",
            "terminal",
            "--out-file",
            harn_out.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert!(harn.stdout.is_empty(), "out_file should suppress stdout");

    let rust = run_eval_prompt_with(
        &[
            template.to_str().unwrap(),
            "--fleet",
            &FLEET.join(","),
            "--output",
            "terminal",
            "--out-file",
            rust_out.to_str().unwrap(),
        ],
        &[("HARN_CLI_IMPL", "rust")],
    );
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert!(rust.stdout.is_empty(), "out_file should suppress stdout");

    let harn_bytes = fs::read_to_string(&harn_out).expect("harn out_file");
    let rust_bytes = fs::read_to_string(&rust_out).expect("rust out_file");
    assert_eq!(
        harn_bytes, rust_bytes,
        "out_file contents diverged\n--- rust ---\n{}\n--- harn ---\n{}",
        rust_bytes, harn_bytes
    );
}

#[test]
fn help_is_byte_identical_between_impls() {
    // Clap intercepts `--help` before either impl ever runs, so the
    // env var has no effect — pin the equality anyway so we catch a
    // future regression where someone routes --help through the wedge.
    let harn = run_eval_prompt_with(&["--help"], &[]);
    let rust = run_eval_prompt_with(&["--help"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(harn.exit_code, 0);
    assert_eq!(rust.exit_code, 0);
    assert_eq!(
        harn.stdout, rust.stdout,
        "--help stdout diverged across impls"
    );
}

// ───────────────────────────────────────────────────────────────────────

struct ProcessOutcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

fn run_eval_prompt(template: &Path, output: &str, extra_env: &[(&str, &str)]) -> ProcessOutcome {
    run_eval_prompt_with(
        &[
            template.to_str().expect("template path utf-8"),
            "--fleet",
            &FLEET.join(","),
            "--output",
            output,
        ],
        extra_env,
    )
}

fn run_eval_prompt_with(argv: &[&str], extra_env: &[(&str, &str)]) -> ProcessOutcome {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harn"));
    cmd.arg("eval").arg("prompt");
    for arg in argv {
        cmd.arg(arg);
    }
    // Drop ambient env that could perturb the renders across the two
    // subprocess invocations: terminal-detection env vars (`NO_COLOR`,
    // `HARN_COLOR`) and any inherited provider keys aren't needed for
    // mock-free render mode and would only add noise to the diff.
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
    let output = cmd.output().expect("spawn harn eval prompt");
    ProcessOutcome {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    }
}
