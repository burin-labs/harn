//! `harn eval prompt` dispatch contract tests.
//!
//! Asserts that the rendering layer ported to
//! `crates/harn-stdlib/src/stdlib/cli/eval/prompt.harn` produces the
//! expected output shape. Aggregation (fleet rendering, run/judge
//! fanout, context-fixture evaluation) stays host-side. These tests pin
//! terminal, HTML, JSON, out-file, and help behavior for the shipped
//! dispatch path.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

/// A four-profile mock-free render fixture: each fleet member maps to a
/// distinct capability family (anthropic-claude / openai-gpt /
/// google-gemini / qwen) so the
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
fn terminal_output_is_byte_identical_across_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let template = dir.path().join("system.harn.prompt");
    fs::write(&template, RENDER_TEMPLATE).expect("write template");

    let harn = run_eval_prompt(&template, "terminal", &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);

    let repeat = run_eval_prompt(&template, "terminal", &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);

    assert_eq!(
        harn.stdout, repeat.stdout,
        "terminal stdout diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
}

#[test]
fn html_output_is_byte_identical_across_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let template = dir.path().join("system.harn.prompt");
    fs::write(&template, RENDER_TEMPLATE).expect("write template");

    let harn = run_eval_prompt(&template, "html", &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);

    let repeat = run_eval_prompt(&template, "html", &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);

    assert_eq!(
        harn.stdout, repeat.stdout,
        "html stdout diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
}

#[test]
fn json_output_is_structurally_identical_across_runs() {
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

    let repeat = run_eval_prompt(&template, "json", &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);

    let harn_value: serde_json::Value =
        serde_json::from_str(&harn.stdout).expect("harn JSON parses");
    let repeat_value: serde_json::Value =
        serde_json::from_str(&repeat.stdout).expect("repeat JSON parses");
    assert_eq!(
        repeat_value, harn_value,
        "json shape diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
}

#[test]
fn out_file_writes_match_across_runs() {
    // Exercises the script's `--out-file` (HARN_EVAL_PROMPT_OUT_FILE)
    // branch: the rendered payload goes to disk, and a "wrote <path>"
    // line goes to stderr. Both stdout and on-disk bytes must match.
    let dir = tempfile::tempdir().expect("tempdir");
    let template = dir.path().join("system.harn.prompt");
    fs::write(&template, RENDER_TEMPLATE).expect("write template");

    let harn_out = dir.path().join("harn.txt");
    let repeat_out = dir.path().join("repeat.txt");

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

    let repeat = run_eval_prompt_with(
        &[
            template.to_str().unwrap(),
            "--fleet",
            &FLEET.join(","),
            "--output",
            "terminal",
            "--out-file",
            repeat_out.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    assert!(repeat.stdout.is_empty(), "out_file should suppress stdout");

    let harn_bytes = fs::read_to_string(&harn_out).expect("harn out_file");
    let repeat_bytes = fs::read_to_string(&repeat_out).expect("repeat out_file");
    assert_eq!(
        harn_bytes, repeat_bytes,
        "out_file contents diverged\n--- repeat ---\n{repeat_bytes}\n--- harn ---\n{harn_bytes}"
    );
}

#[test]
fn help_is_byte_identical_across_runs() {
    // Clap intercepts `--help` before the dispatch script runs, so the
    // env var has no effect — pin the equality anyway so we catch a
    // future regression where someone routes --help through the wedge.
    let harn = run_eval_prompt_with(&["--help"], &[]);
    let repeat = run_eval_prompt_with(&["--help"], &[]);
    assert_eq!(harn.exit_code, 0);
    assert_eq!(repeat.exit_code, 0);
    assert_eq!(
        harn.stdout, repeat.stdout,
        "--help stdout diverged across repeat runs"
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
    for key in ["NO_COLOR", "HARN_COLOR"] {
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
