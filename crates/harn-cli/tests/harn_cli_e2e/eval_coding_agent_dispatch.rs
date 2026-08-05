//! `harn eval coding-agent` dispatch contract tests.
//!
//! The rendering layer for `harn eval coding-agent` ships in
//! `crates/harn-stdlib/src/stdlib/cli/eval/coding_agent.harn`. This
//! test asserts the expected shape for every output the dispatch path owns:
//!
//!   * `summary.md` — byte-identical (the .harn renderer mirrors the
//!     host markdown writer line-for-line).
//!   * `followups.md` — byte-identical (same reasoning).
//!   * The post-run one-line stdout summary
//!     (`coding-agent eval: ...`) — byte-identical.
//!   * The `--json` pretty stdout payload — structurally identical
//!     (Harn's `json_stringify_pretty` sorts dict keys alphabetically;
//!     serde emits struct fields in declaration order).
//!   * The on-disk `summary.json` artifact — byte-identical across
//!     runs (the artifact always stays on the serde-driven host path
//!     because hosted ingestion + the experiment driver in
//!     `experiments/step-judge/run.sh` both depend on the serde
//!     struct-field byte order). This guards against an accidental
//!     future port that routes the JSON artifact through Harn's
//!     alphabetical-key serialiser.
//!
//! Aggregation (matrix execution, `execute_run` fanout, scoring,
//! rollups, comparisons, follow-up suggestions, Ollama snapshot) stays
//! host-side. These tests pin text/markdown bytes where output is
//! deterministic and structural equality for the JSON stdout path.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn summary_md_is_byte_identical_across_runs_after_normalizing_metrics() {
    // Use a single shared output dir for repeated runs. The aggregated
    // report bakes the output_dir + per-run transcript paths into the
    // rendered tables, so reusing the same dir keeps those strings
    // identical between the two subprocess runs. Sequential reset_dir
    // handles the per-run dir overwrites without interfering.
    let dir = tempfile::tempdir().expect("tempdir");
    let shared_out = dir.path().join("bench");

    let harn = run_eval_coding_agent(&shared_out, false, &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_md = fs::read_to_string(shared_out.join("summary.md")).expect("harn summary.md");

    let repeat = run_eval_coding_agent(&shared_out, false, &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    let repeat_md = fs::read_to_string(shared_out.join("summary.md")).expect("repeat summary.md");

    let harn_md = normalize_summary_markdown_metrics(&harn_md);
    let repeat_md = normalize_summary_markdown_metrics(&repeat_md);
    assert_eq!(
        harn_md, repeat_md,
        "summary.md diverged\n--- repeat ---\n{repeat_md}\n--- harn ---\n{harn_md}"
    );
}

#[test]
fn followups_md_is_byte_identical_across_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shared_out = dir.path().join("bench");

    let harn = run_eval_coding_agent(&shared_out, false, &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_md = fs::read_to_string(shared_out.join("followups.md")).expect("harn followups.md");

    let repeat = run_eval_coding_agent(&shared_out, false, &[]);
    assert_eq!(repeat.exit_code, 0, "repeat stderr={}", repeat.stderr);
    let repeat_md =
        fs::read_to_string(shared_out.join("followups.md")).expect("repeat followups.md");

    assert_eq!(
        harn_md, repeat_md,
        "followups.md diverged\n--- repeat ---\n{repeat_md}\n--- harn ---\n{harn_md}"
    );
}

#[test]
fn stdout_summary_line_is_byte_identical_across_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let harn_out = dir.path().join("harn");
    let repeat_out = dir.path().join("repeat");

    let harn = run_eval_coding_agent(&harn_out, false, &[]);
    let repeat = run_eval_coding_agent(&repeat_out, false, &[]);
    assert_eq!(
        harn.stdout, repeat.stdout,
        "stdout summary line diverged\n--- repeat ---\n{}\n--- harn ---\n{}",
        repeat.stdout, harn.stdout
    );
}

#[test]
fn json_stdout_is_structurally_identical_across_runs() {
    // Harn's `json_stringify_pretty` sorts dict keys alphabetically;
    // serde's `to_string_pretty` emits struct fields in declaration
    // order. The wire byte order can therefore differ even though the
    // parsed shapes match — assert structural equality, not byte
    // identity, for the JSON path.
    //
    // We normalise volatile execution metrics across both runs because
    // the matrix executor measures wall-clock per cell and fresh agent
    // session ids can perturb token estimates by a token or two. The
    // fields owned by the renderer remain structurally compared.
    let dir = tempfile::tempdir().expect("tempdir");
    let shared_out = dir.path().join("bench");

    let harn = run_eval_coding_agent(&shared_out, true, &[]);
    let repeat = run_eval_coding_agent(&shared_out, true, &[]);

    let mut harn_value: serde_json::Value =
        serde_json::from_str(&harn.stdout).expect("harn --json stdout parses");
    let mut repeat_value: serde_json::Value =
        serde_json::from_str(&repeat.stdout).expect("repeat --json stdout parses");
    zero_volatile_metrics(&mut harn_value);
    zero_volatile_metrics(&mut repeat_value);
    assert_eq!(
        repeat_value, harn_value,
        "--json stdout diverged structurally"
    );
}

#[test]
fn summary_json_artifact_keeps_serde_struct_field_order_across_runs() {
    // The on-disk `summary.json` is consumed by hosted ingestion + the
    // experiment driver in `experiments/step-judge/run.sh`, both of
    // which depend on serde's struct-field order. The .harn rendering
    // port intentionally leaves this artifact on the host path — this
    // test guards against an accidental future port that routes the
    // JSON artifact through Harn's alphabetical-key serialiser.
    //
    // We assert that the top-level keys appear in serde declaration
    // order (not alphabetical) on repeated runs. We also assert structural
    // equality (with timing fields zeroed) so a future divergence in
    // any field surfaces here.
    let dir = tempfile::tempdir().expect("tempdir");
    let shared_out = dir.path().join("bench");

    run_eval_coding_agent(&shared_out, false, &[]);
    let harn_json = fs::read_to_string(shared_out.join("summary.json")).expect("harn summary.json");

    run_eval_coding_agent(&shared_out, false, &[]);
    let repeat_json =
        fs::read_to_string(shared_out.join("summary.json")).expect("repeat summary.json");

    // Both files should start with `schema_version` (the first
    // serde-declared field on EvalSummary) and `fixture_ids` second —
    // confirming serde struct-field order, not alphabetical-key order.
    for (label, body) in [("harn", &harn_json), ("repeat", &repeat_json)] {
        let first_field_pos = body.find("\"schema_version\"").unwrap_or(usize::MAX);
        let fixture_ids_pos = body.find("\"fixture_ids\"").unwrap_or(usize::MAX);
        assert!(
            first_field_pos < fixture_ids_pos,
            "{label} summary.json top-level keys should follow serde struct-field order \
             (schema_version before fixture_ids), but got {body}"
        );
    }

    let mut harn_value: serde_json::Value =
        serde_json::from_str(&harn_json).expect("harn summary.json parses");
    let mut repeat_value: serde_json::Value =
        serde_json::from_str(&repeat_json).expect("repeat summary.json parses");
    zero_volatile_metrics(&mut harn_value);
    zero_volatile_metrics(&mut repeat_value);
    assert_eq!(
        harn_value, repeat_value,
        "summary.json diverged structurally"
    );
}

// ─── helpers ─────────────────────────────────────────────────────────────

/// Walk a `serde_json::Value` tree and zero volatile metrics so two
/// invocations of the same coding-agent matrix produce structurally
/// equal reports. The renderer contract tests compare presentation-owned
/// fields; wall-clock timings and token estimates belong to execution.
fn zero_volatile_metrics(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if matches!(
                    key.as_str(),
                    "elapsed_ms"
                        | "duration_ms"
                        | "input_tokens"
                        | "output_tokens"
                        | "token_delta_text_minus_native"
                ) {
                    *child = serde_json::Value::Number(serde_json::Number::from(0));
                } else {
                    zero_volatile_metrics(child);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                zero_volatile_metrics(item);
            }
        }
        _ => {}
    }
}

fn normalize_summary_markdown_metrics(markdown: &str) -> String {
    let mut out = String::new();
    for line in markdown.lines() {
        if line.starts_with("| `") {
            let mut cells = line.split('|').collect::<Vec<_>>();
            match cells.len() {
                // Runs table: normalize the `tokens` column.
                15 => cells[10] = " <tokens> ",
                // Native/Text Comparison table: normalize `token delta`.
                13 => cells[9] = " <token-delta> ",
                _ => {}
            }
            out.push_str(&cells.join("|"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

struct ProcessOutcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

/// Invoke `harn eval coding-agent` in mock-only matrix mode. The mock
/// provider has no LLM credentials, so this should pass without network
/// or provider setup; the rendered outputs must match across runs.
fn run_eval_coding_agent(output: &Path, json: bool, extra_env: &[(&str, &str)]) -> ProcessOutcome {
    let mut argv = vec![
        "eval".to_string(),
        "coding-agent".to_string(),
        "--model".to_string(),
        "mock:mock".to_string(),
        "--tool-format".to_string(),
        "native,text".to_string(),
        "--max-runs".to_string(),
        "2".to_string(),
        "--output".to_string(),
        output.display().to_string(),
    ];
    if json {
        argv.push("--json".to_string());
    }
    run_harn(&argv, extra_env)
}

fn run_harn(argv: &[String], extra_env: &[(&str, &str)]) -> ProcessOutcome {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harn"));
    for arg in argv {
        cmd.arg(arg);
    }
    // Drop ambient env that could perturb the renders across the two
    // subprocess invocations.
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
    let output = cmd.output().expect("spawn harn eval coding-agent");
    ProcessOutcome {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
    }
}

#[test]
fn eval_coding_agent_surfaces_tool_format_override_warning() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let output = tmp.path().join("bench");
    let argv = vec![
        "eval".to_string(),
        "coding-agent".to_string(),
        "--fixture".to_string(),
        "no-tool-diagnosis".to_string(),
        "--model".to_string(),
        "mock:claude-opus-4-7".to_string(),
        "--tool-format".to_string(),
        "text".to_string(),
        "--max-runs".to_string(),
        "1".to_string(),
        "--max-iterations".to_string(),
        "1".to_string(),
        "--override-reason".to_string(),
        "compare text trace".to_string(),
        "--output".to_string(),
        output.display().to_string(),
    ];
    let outcome = run_harn(&argv, &[]);
    assert_eq!(
        outcome.exit_code, 0,
        "eval failed\nstdout={}\nstderr={}",
        outcome.stdout, outcome.stderr
    );
    assert!(
        outcome.stderr.contains(
            "warning: tool_format override: mock:claude-opus-4-7 requested text over recommended native"
        ),
        "stderr should surface the override warning; got:\n{}",
        outcome.stderr
    );
}
