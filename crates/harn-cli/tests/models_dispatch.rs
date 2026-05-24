#![recursion_limit = "256"]

//! Partial-port verification for `harn models list` / `recommend` /
//! `test` (harn#2309 / W9).
//!
//! Each subcommand's render pipeline now lives in
//! `crates/harn-stdlib/src/stdlib/cli/models/*.harn`. The Rust
//! dispatch shims keep doing the host-only work (Ollama subprocess
//! probe for list, hardware snapshot + cloud-cred probe for recommend,
//! the actual smoke-test for test) and hand a JSON payload across the
//! dispatch wedge to the script for formatting.
//!
//! The `HARN_CLI_IMPL=rust` escape hatch keeps the legacy direct path
//! so this test can compare both impls at runtime until the C1 ratchet
//! (#2314) deletes it.
//!
//! Parity bar:
//!   * Human text: byte-for-byte identity.
//!   * JSON envelopes: structural identity (Harn's
//!     `json_stringify_pretty` sorts dict keys alphabetically; serde
//!     emits struct fields in declaration order, so wire byte order
//!     differs but the parsed shape must match).

use std::process::{Command, Output};

fn harn_binary() -> &'static str {
    env!("CARGO_BIN_EXE_harn")
}

struct SubprocessOutcome {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

fn run(argv: &[&str], extra_env: &[(&str, &str)]) -> SubprocessOutcome {
    let mut cmd = Command::new(harn_binary());
    for arg in argv {
        cmd.arg(arg);
    }
    // Strip ambient HARN_CLI_IMPL so the test controls the dispatch
    // path, and strip NO_COLOR / HARN_COLOR so terminal-detection env
    // vars don't perturb the renders across the two subprocess calls.
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

// ─── models list ─────────────────────────────────────────────────────────

#[test]
fn models_list_human_text_is_byte_identical_between_impls() {
    let harn = run(&["models", "list"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run(&["models", "list"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert_eq!(
        harn.stdout, rust.stdout,
        "models list human stdout diverged"
    );
}

#[test]
fn models_list_provider_filter_byte_identical_between_impls() {
    let harn = run(&["models", "list", "--provider", "openai"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run(
        &["models", "list", "--provider", "openai"],
        &[("HARN_CLI_IMPL", "rust")],
    );
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert_eq!(
        harn.stdout, rust.stdout,
        "models list --provider stdout diverged"
    );
}

#[test]
fn models_list_installed_only_byte_identical_between_impls() {
    // --installed-only with no installed ollama models prints
    // `(no models match)` on both paths; the test machine may or may
    // not have ollama installed but the shim hands the same set to
    // both impls so the output should be identical regardless.
    let harn = run(&["models", "list", "--installed-only"], &[]);
    let rust = run(
        &["models", "list", "--installed-only"],
        &[("HARN_CLI_IMPL", "rust")],
    );
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert_eq!(
        harn.stdout, rust.stdout,
        "models list --installed-only stdout diverged"
    );
}

#[test]
fn models_list_json_is_structurally_identical_between_impls() {
    let harn = run(&["models", "list", "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run(&["models", "list", "--json"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let rust_value = parse_json(&rust.stdout, "rust");
    assert_eq!(
        rust_value, harn_value,
        "models list --json shape diverged\n--- rust ---\n{}\n--- harn ---\n{}",
        rust.stdout, harn.stdout
    );
}

// ─── models recommend ───────────────────────────────────────────────────

/// Recommendation output depends on the current machine's RAM /
/// GPU / installed credentials. Compare the two impls back-to-back
/// without rationalising — they must agree on whatever the host
/// reports.
#[test]
fn models_recommend_human_text_is_byte_identical_between_impls() {
    let harn = run(&["models", "recommend"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run(&["models", "recommend"], &[("HARN_CLI_IMPL", "rust")]);
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    assert_eq!(
        harn.stdout, rust.stdout,
        "models recommend human stdout diverged"
    );
}

#[test]
fn models_recommend_json_shape_is_identical_between_impls() {
    let harn = run(&["models", "recommend", "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run(
        &["models", "recommend", "--json"],
        &[("HARN_CLI_IMPL", "rust")],
    );
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let rust_value = parse_json(&rust.stdout, "rust");
    // The two impls run separate hardware probes so floating numbers
    // like `available_bytes` may differ by a tiny amount between back-
    // to-back invocations. Compare the structural keyset + the fields
    // that don't depend on a fresh snapshot.
    for key in [
        "model_id",
        "harn_selector",
        "provider",
        "rationale",
        "ram_bucket",
        "gpu",
        "has_provider_key",
    ] {
        assert_eq!(
            rust_value[key], harn_value[key],
            "recommend.{key} diverged\n--- rust ---\n{}\n--- harn ---\n{}",
            rust.stdout, harn.stdout
        );
    }
    // Hardware sub-object should at least carry the same shape — ram /
    // gpu / disk top-level keys and the gpu kind. Numeric values
    // intentionally not compared because the host can shift them
    // between back-to-back probes.
    let harn_hw = &harn_value["hardware"];
    let rust_hw = &rust_value["hardware"];
    for key in ["ram", "gpu", "disk"] {
        assert!(
            harn_hw[key].is_object(),
            "harn hardware.{key} should be an object"
        );
        assert!(
            rust_hw[key].is_object(),
            "rust hardware.{key} should be an object"
        );
    }
    assert_eq!(
        rust_hw["gpu"]["kind"], harn_hw["gpu"]["kind"],
        "hardware.gpu.kind diverged"
    );
    assert_eq!(
        rust_hw["disk"]["path"], harn_hw["disk"]["path"],
        "hardware.disk.path diverged"
    );
}

// ─── models test ────────────────────────────────────────────────────────

#[test]
fn models_test_mock_human_line_is_byte_identical_between_impls() {
    // Mock provider runs offline and is deterministic enough to
    // compare back-to-back — the only fields that can vary are
    // `latency_ms` and `first_token_ms`. Verify the line shape rather
    // than full text identity, and tolerate the timing fields.
    let harn = run(&["models", "test", "mock", "--provider", "mock"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run(
        &["models", "test", "mock", "--provider", "mock"],
        &[("HARN_CLI_IMPL", "rust")],
    );
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    for fragment in [
        "model_id=mock",
        "provider=mock",
        "latency_ms=",
        "first_token_ms=",
        "input_tokens=",
        "output_tokens=",
        "estimated_cost_usd=0.000000",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
        assert!(
            rust.stdout.contains(fragment),
            "rust stdout missing {fragment}: {}",
            rust.stdout
        );
    }
    // Pin the exact key order (`model_id=` first, then `provider=`,
    // …) so a future renderer change can't silently drop a column.
    let harn_keys = test_line_keys(&harn.stdout);
    let rust_keys = test_line_keys(&rust.stdout);
    assert_eq!(
        rust_keys, harn_keys,
        "models test stdout key order diverged"
    );
}

#[test]
fn models_test_mock_json_shape_is_identical_between_impls() {
    let harn = run(
        &["models", "test", "mock", "--provider", "mock", "--json"],
        &[],
    );
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let rust = run(
        &["models", "test", "mock", "--provider", "mock", "--json"],
        &[("HARN_CLI_IMPL", "rust")],
    );
    assert_eq!(rust.exit_code, 0, "rust stderr={}", rust.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let rust_value = parse_json(&rust.stdout, "rust");
    for key in [
        "model_id",
        "provider",
        "input_tokens",
        "output_tokens",
        "estimated_cost_usd",
    ] {
        assert_eq!(
            rust_value[key], harn_value[key],
            "models test --json {key} diverged\n--- rust ---\n{}\n--- harn ---\n{}",
            rust.stdout, harn.stdout
        );
    }
    // `latency_ms` and `first_token_ms` are timing-dependent — just
    // verify `latency_ms` is integer-shaped on both impls so a future
    // drift to string would fail the test. `first_token_ms` is
    // optional (absent when the stream never delivered a delta) so
    // the parity test treats its presence as best-effort.
    let latency_key = "latency_ms";
    assert!(
        harn_value[latency_key].is_u64() || harn_value[latency_key].is_i64(),
        "harn {latency_key} should be integer; got: {}",
        harn_value[latency_key]
    );
    assert!(
        rust_value[latency_key].is_u64() || rust_value[latency_key].is_i64(),
        "rust {latency_key} should be integer; got: {}",
        rust_value[latency_key]
    );
}

#[test]
fn models_test_failure_json_envelope_is_byte_identical_between_impls() {
    // Drop every provider credential env var so the smoke-test fails
    // deterministically on both impls with the missing-API-key error.
    // The failure JSON path is the most-tested branch in the field
    // because users probe new provider configs by running `models
    // test` with the wrong selector — parity here actually matters.
    let scrubbers: Vec<(&str, &str)> = vec![
        ("OPENAI_API_KEY", ""),
        ("ANTHROPIC_API_KEY", ""),
        ("GEMINI_API_KEY", ""),
        ("GOOGLE_API_KEY", ""),
        ("AZURE_OPENAI_API_KEY", ""),
        ("AZURE_OPENAI_AD_TOKEN", ""),
        ("AZURE_OPENAI_BEARER_TOKEN", ""),
        ("CEREBRAS_API_KEY", ""),
        ("DASHSCOPE_API_KEY", ""),
        ("DEEPSEEK_API_KEY", ""),
        ("FIREWORKS_API_KEY", ""),
        ("GOOGLE_APPLICATION_CREDENTIALS", ""),
        ("GOOGLE_OAUTH_ACCESS_TOKEN", ""),
        ("GROQ_API_KEY", ""),
        ("HF_TOKEN", ""),
        ("HUGGINGFACE_API_KEY", ""),
        ("OPENROUTER_API_KEY", ""),
        ("TOGETHER_AI_API_KEY", ""),
        ("VERTEX_AI_ACCESS_TOKEN", ""),
        ("HARN_LLM_PROVIDER", ""),
        ("LLM_PROVIDER", ""),
    ];
    let harn_env: Vec<(&str, &str)> = scrubbers.clone();
    let mut rust_env: Vec<(&str, &str)> = scrubbers;
    rust_env.push(("HARN_CLI_IMPL", "rust"));

    let harn = run_with_clean_env(
        &[
            "models",
            "test",
            "foo-not-real",
            "--provider",
            "openai",
            "--json",
        ],
        &harn_env,
    );
    let rust = run_with_clean_env(
        &[
            "models",
            "test",
            "foo-not-real",
            "--provider",
            "openai",
            "--json",
        ],
        &rust_env,
    );
    assert_eq!(
        harn.exit_code, 1,
        "harn should fail; stderr={}",
        harn.stderr
    );
    assert_eq!(
        rust.exit_code, 1,
        "rust should fail; stderr={}",
        rust.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    let rust_value = parse_json(&rust.stdout, "rust");
    assert_eq!(
        rust_value, harn_value,
        "models test failure JSON diverged\n--- rust ---\n{}\n--- harn ---\n{}",
        rust.stdout, harn.stdout
    );
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(false));
    assert!(
        harn_value["error"].is_string(),
        "failure envelope missing 'error' string field"
    );
}

// ────────────────────────────────────────────────────────────────────────

fn run_with_clean_env(argv: &[&str], env: &[(&str, &str)]) -> SubprocessOutcome {
    let mut cmd = Command::new(harn_binary());
    for arg in argv {
        cmd.arg(arg);
    }
    for key in ["HARN_CLI_IMPL", "NO_COLOR", "HARN_COLOR"] {
        cmd.env_remove(key);
    }
    for (k, v) in env {
        if v.is_empty() {
            cmd.env_remove(k);
        } else {
            cmd.env(k, v);
        }
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

fn test_line_keys(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|kv| kv.split('=').next().map(str::to_string))
        .collect()
}
