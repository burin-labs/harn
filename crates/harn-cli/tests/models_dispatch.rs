#![recursion_limit = "256"]

//! Contract checks for the Harn-rendered `harn models` subcommands.
//!
//! Each subcommand's render pipeline now lives in
//! `crates/harn-stdlib/src/stdlib/cli/models/*.harn`. The Rust
//! dispatch shims keep doing the host-only work (Ollama subprocess
//! probe for list, hardware snapshot + cloud-cred probe for recommend,
//! the actual smoke-test for test) and hand a JSON payload across the
//! dispatch wedge to the script for formatting.

use std::fs;
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
    // Strip terminal-detection env vars so they don't perturb renders.
    for key in ["NO_COLOR", "HARN_COLOR"] {
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

// - models list ------------------------------------------------------------

#[test]
fn models_list_human_text_renders_catalog_groups() {
    let harn = run(&["models", "list"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert!(
        harn.stdout.contains("anthropic\n"),
        "stdout={}",
        harn.stdout
    );
    assert!(
        harn.stdout.contains("  claude-haiku-4-5-20251001"),
        "stdout={}",
        harn.stdout
    );
}

#[test]
fn models_list_provider_filter_limits_groups() {
    let harn = run(&["models", "list", "--provider", "openai"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert!(
        harn.stdout.starts_with("openai\n"),
        "stdout={}",
        harn.stdout
    );
    assert!(!harn.stdout.contains("\nmock\n"), "stdout={}", harn.stdout);
}

#[test]
fn models_list_installed_only_is_well_formed() {
    let harn = run(&["models", "list", "--installed-only"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert!(!harn.stdout.trim().is_empty(), "stdout should not be empty");
}

#[test]
fn models_list_json_has_provider_array() {
    let harn = run(&["models", "list", "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let providers = harn_value["providers"].as_array().expect("providers array");
    let anthropic = providers
        .iter()
        .find(|provider| provider["name"] == "anthropic")
        .expect("anthropic provider group");
    let models = anthropic["models"].as_array().expect("anthropic models");
    assert!(
        models
            .iter()
            .any(|model| model["id"] == "claude-haiku-4-5-20251001"),
        "anthropic models={models:?}"
    );
}

// - models recommend ------------------------------------------------------

#[test]
fn models_recommend_human_text_has_model_and_rationale() {
    let harn = run(&["models", "recommend"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let lines: Vec<&str> = harn.stdout.lines().collect();
    assert!(lines.len() >= 2, "stdout={}", harn.stdout);
    assert!(!lines[0].trim().is_empty(), "stdout={}", harn.stdout);
    assert!(lines[1].contains("->"), "stdout={}", harn.stdout);
}

#[test]
fn models_recommend_json_shape_is_stable() {
    let harn = run(&["models", "recommend", "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    for key in [
        "model_id",
        "harn_selector",
        "provider",
        "rationale",
        "ram_bucket",
        "gpu",
        "has_provider_key",
    ] {
        assert!(!harn_value[key].is_null(), "missing recommend.{key}");
    }
    let harn_hw = &harn_value["hardware"];
    for key in ["ram", "gpu", "disk"] {
        assert!(
            harn_hw[key].is_object(),
            "harn hardware.{key} should be an object"
        );
    }
    assert!(harn_value["rationale"]
        .as_str()
        .unwrap_or("")
        .contains("->"));
}

// - models test -----------------------------------------------------------

#[test]
fn models_test_mock_human_line_shape_is_stable() {
    let harn = run(&["models", "test", "mock", "--provider", "mock"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "model_id=mock",
        "provider=mock",
        "latency_ms=",
        "first_token_ms=",
        "input_tokens=",
        "output_tokens=",
        "estimated_cost_usd=0",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
    let keys = test_line_keys(&harn.stdout);
    assert_eq!(
        keys,
        vec![
            "model_id",
            "provider",
            "latency_ms",
            "first_token_ms",
            "input_tokens",
            "output_tokens",
            "estimated_cost_usd",
        ],
        "models test stdout key order diverged"
    );
}

#[test]
fn models_test_mock_json_shape_is_stable() {
    let harn = run(
        &["models", "test", "mock", "--provider", "mock", "--json"],
        &[],
    );
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    for key in [
        "model_id",
        "provider",
        "input_tokens",
        "output_tokens",
        "estimated_cost_usd",
    ] {
        assert!(!harn_value[key].is_null(), "missing models test {key}");
    }
    let latency_key = "latency_ms";
    assert!(
        harn_value[latency_key].is_u64() || harn_value[latency_key].is_i64(),
        "harn {latency_key} should be integer; got: {}",
        harn_value[latency_key]
    );
    assert_eq!(harn_value["estimated_cost_usd"].as_f64(), Some(0.0));
}

#[test]
fn models_test_failure_json_envelope_is_stable() {
    // Drop every provider credential env var so the smoke-test fails
    // deterministically with the missing-API-key error.
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
    let harn = run_with_clean_env(
        &[
            "models",
            "test",
            "foo-not-real",
            "--provider",
            "openai",
            "--json",
        ],
        &scrubbers,
    );
    assert_eq!(
        harn.exit_code, 1,
        "harn should fail; stderr={}",
        harn.stderr
    );
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(false));
    assert!(
        harn_value["error"].is_string(),
        "failure envelope missing 'error' string field"
    );
}

// - models lora inspect ---------------------------------------------------

#[test]
fn models_lora_inspect_human_text_includes_launch_hint() {
    let adapter = write_lora_adapter_fixture();
    let adapter_path = adapter.path().display().to_string();
    let harn = run(
        &[
            "models",
            "lora",
            "inspect",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--name",
            "burin-tools",
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "burin-tools -> gemma-4-e4b-it via vllm",
        "base match: same basename",
        "tool format: json",
        "native tools: no, preferred: unset",
        "catalog LoRA launch flags: yes",
        "catalog LoRA rank flag: yes",
        "max LoRA rank: 16",
        "harn local launch local-gemma4-e4b --provider vllm",
        "--model-source google/gemma-4-e4b-it",
        "--lora-adapter burin-tools=",
        "--max-lora-rank 16",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_inspect_json_shape_is_stable() {
    let adapter = write_lora_adapter_fixture();
    let adapter_path = adapter.path().display().to_string();
    let harn = run(
        &[
            "models",
            "lora",
            "inspect",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--name",
            "burin-tools",
            "--json",
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(true));
    assert_eq!(harn_value["base"]["selector"], "local-gemma4-e4b");
    assert_eq!(harn_value["base"]["id"], "gemma-4-e4b-it");
    assert_eq!(harn_value["base"]["provider"], "vllm");
    assert_eq!(harn_value["base"]["tool_format"], "json");
    assert_eq!(harn_value["adapter"]["name"], "burin-tools");
    assert_eq!(harn_value["adapter"]["peft_type"], "LORA");
    assert_eq!(harn_value["compatibility"]["base_model_match"], "suffix");
    assert_eq!(
        harn_value["compatibility"]["provider_supports_lora_launch"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        harn_value["compatibility"]["provider_supports_lora_max_rank"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(harn_value["tool_calling"]["native_tools"], false);
    assert_eq!(harn_value["launch"]["request_model"], "burin-tools");
    assert_eq!(harn_value["launch"]["max_lora_rank"].as_u64(), Some(16));
    let launch = harn_value["launch"]["harn_local_launch"]
        .as_array()
        .expect("launch argv");
    assert!(
        launch.iter().any(|arg| arg == "--lora-adapter"),
        "launch argv={launch:?}"
    );
    assert!(
        launch
            .windows(2)
            .any(|pair| pair[0] == "--max-lora-rank" && pair[1] == "16"),
        "launch argv={launch:?}"
    );
}

// ────────────────────────────────────────────────────────────────────────

fn write_lora_adapter_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(tmp.path().join("adapter_model.safetensors"), b"stub").expect("adapter weights");
    fs::write(
        tmp.path().join("adapter_config.json"),
        r#"{
            "peft_type": "LORA",
            "base_model_name_or_path": "google/gemma-4-e4b-it",
            "task_type": "CAUSAL_LM",
            "r": 16,
            "lora_alpha": 32,
            "target_modules": ["q_proj", "v_proj"]
        }"#,
    )
    .expect("adapter config");
    tmp
}

fn run_with_clean_env(argv: &[&str], env: &[(&str, &str)]) -> SubprocessOutcome {
    let mut cmd = Command::new(harn_binary());
    for arg in argv {
        cmd.arg(arg);
    }
    for key in ["NO_COLOR", "HARN_COLOR"] {
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
