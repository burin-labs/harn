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

fn success_data(value: &serde_json::Value) -> &serde_json::Value {
    assert_eq!(value["ok"], serde_json::Value::Bool(true));
    value["data"].as_object().expect("success envelope data");
    &value["data"]
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

// - models batch ----------------------------------------------------------

#[test]
fn models_batch_plan_reports_harn_live_adapter_support() {
    let harn = run(&["models", "batch", "plan", "--json"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "batch plan");
    let report = success_data(&harn_value);
    let models = report["models"].as_array().expect("models");
    let openai = models
        .iter()
        .find(|model| model["provider"] == "openai")
        .expect("openai batch model");
    assert_eq!(openai["batch"]["wire_format"], "openai");
    assert_eq!(openai["batch"]["max_requests"], 50_000);
    assert_eq!(openai["batch"]["max_input_bytes"], 209_715_200);
    assert_eq!(openai["batch"]["result_retention_days"], 30);
    assert_eq!(openai["batch"]["result_ordering"], "custom_id_rejoin");
    assert_eq!(openai["batch"]["partial_failure"], "per_request");
    assert_eq!(openai["batch"]["cancellation"], "supported");
    assert!(
        openai["batch"]["security_notes"]
            .as_array()
            .is_some_and(|notes| !notes.is_empty()),
        "OpenAI batch plan should include public storage/security notes"
    );
    assert!(
        openai["batch"]["operational_notes"]
            .as_array()
            .is_some_and(|notes| notes
                .iter()
                .any(|note| note.as_str().unwrap_or("").contains("one model"))),
        "OpenAI batch plan should include provider grouping constraints"
    );
    assert_eq!(openai["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(openai["batch"]["harn_live_adapter"]["status"], true);
    assert_eq!(openai["batch"]["harn_live_adapter"]["cancel"], true);
    assert_eq!(openai["batch"]["harn_live_adapter"]["download"], true);

    let xai = models
        .iter()
        .find(|model| model["provider"] == "xai")
        .expect("xai batch model");
    assert_eq!(xai["batch"]["wire_format"], "xai");
    assert_eq!(xai["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(xai["batch"]["harn_live_adapter"]["status"], true);
    assert_eq!(xai["batch"]["harn_live_adapter"]["cancel"], false);
    assert_eq!(xai["batch"]["harn_live_adapter"]["download"], true);

    let groq = models
        .iter()
        .find(|model| model["provider"] == "groq")
        .expect("groq batch model");
    assert_eq!(groq["batch"]["wire_format"], "openai");
    assert_eq!(groq["batch"]["discount_percent"], 50);
    assert_eq!(groq["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(groq["batch"]["harn_live_adapter"]["status"], true);
    assert_eq!(groq["batch"]["harn_live_adapter"]["cancel"], false);
    assert_eq!(groq["batch"]["harn_live_adapter"]["download"], true);

    let together = models
        .iter()
        .find(|model| model["provider"] == "together")
        .expect("together batch model");
    assert_eq!(together["batch"]["wire_format"], "openai");
    assert_eq!(together["batch"]["discount_percent"], 50);
    assert_eq!(together["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(together["batch"]["harn_live_adapter"]["status"], true);
    assert_eq!(together["batch"]["harn_live_adapter"]["cancel"], false);
    assert_eq!(together["batch"]["harn_live_adapter"]["download"], true);

    let gemini = models
        .iter()
        .find(|model| model["provider"] == "gemini")
        .expect("gemini batch model");
    assert_eq!(gemini["batch"]["wire_format"], "gemini");
    assert_eq!(gemini["batch"]["max_input_bytes"], 2_147_483_648_u64);
    assert_eq!(gemini["batch"]["result_ordering"], "custom_id_rejoin");
    assert_eq!(gemini["batch"]["partial_failure"], "per_request");
    assert_eq!(gemini["batch"]["cancellation"], "supported");
    assert!(
        gemini["batch"]["operational_notes"]
            .as_array()
            .is_some_and(|notes| notes
                .iter()
                .any(|note| note.as_str().unwrap_or("").contains("not idempotent"))),
        "Gemini batch plan should surface create-retry idempotency risk"
    );
    assert_eq!(gemini["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(gemini["batch"]["harn_live_adapter"]["status"], true);
    assert_eq!(gemini["batch"]["harn_live_adapter"]["cancel"], true);
    assert_eq!(gemini["batch"]["harn_live_adapter"]["download"], true);

    let fireworks = models
        .iter()
        .find(|model| model["provider"] == "fireworks")
        .expect("fireworks batch model");
    assert_eq!(fireworks["batch"]["wire_format"], "fireworks");
    assert_eq!(fireworks["batch"]["discount_percent"], 50);
    assert!(
        fireworks["batch"]["operational_notes"]
            .as_array()
            .is_some_and(|notes| notes
                .iter()
                .any(|note| note.as_str().unwrap_or("").contains("model-specific"))),
        "Fireworks batch plan should surface model-specific capability constraints"
    );
    assert_eq!(fireworks["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(fireworks["batch"]["harn_live_adapter"]["status"], true);
    assert_eq!(fireworks["batch"]["harn_live_adapter"]["cancel"], false);
    assert_eq!(fireworks["batch"]["harn_live_adapter"]["download"], true);

    let human = run(&["models", "batch", "plan", "--provider", "gemini"], &[]);
    assert_eq!(human.exit_code, 0, "harn stderr={}", human.stderr);
    assert!(human.stdout.contains("live submit"), "{}", human.stdout);
}

#[test]
fn models_batch_manifest_and_dry_run_together_openai_compatible() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests_path = tmp.path().join("requests.jsonl");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &requests_path,
        r#"{"custom_id":"together-case-1","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
"#,
    )
    .expect("write requests");

    let manifest = run(
        &[
            "models",
            "batch",
            "manifest",
            "--provider",
            "together",
            "--model",
            "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            "--requests",
            requests_path.to_str().expect("utf8 requests path"),
            "--out",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--json",
        ],
        &[],
    );
    assert_eq!(manifest.exit_code, 0, "harn stderr={}", manifest.stderr);

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "together batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "together");
    assert_eq!(job["batch"]["wire_format"], "openai");
    assert_eq!(job["batch"]["discount_percent"], 50);
    assert_eq!(job["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(job["endpoint"], "/v1/chat/completions");
    assert_eq!(job["submit"]["operation"], "POST /v1/batches");
    assert_eq!(job["submit"]["upload"]["purpose"], "batch-api");
    assert_eq!(
        job["submit"]["create_batch"]["input_file_id"],
        "<uploaded-file-id>"
    );

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("together line"),
        "together batch line",
    );
    assert_eq!(request["custom_id"], "together-case-1");
    assert_eq!(request["method"], "POST");
    assert_eq!(request["url"], "/v1/chat/completions");
    assert_eq!(
        request["body"]["model"],
        "meta-llama/Llama-3.3-70B-Instruct-Turbo"
    );

    let receipt_path = report["receipt"].as_str().expect("receipt path");
    let submission_path = tmp.path().join("submission.json");
    let submitted = run(
        &[
            "models",
            "batch",
            "submit",
            "--receipt",
            receipt_path,
            "--out",
            submission_path.to_str().expect("utf8 submission path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(submitted.exit_code, 0, "harn stderr={}", submitted.stderr);
    let submitted_value = parse_json(&submitted.stdout, "together batch submit");
    let submission = success_data(&submitted_value);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(submitted_job["provider"], "together");
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "TOGETHER_AI_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        "https://api.together.xyz/v1"
    );

    let status_path = tmp.path().join("status.json");
    let status = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission_path.to_str().expect("utf8 submission path"),
            "--out",
            status_path.to_str().expect("utf8 status path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(status.exit_code, 0, "harn stderr={}", status.stderr);

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        "together status receipt",
    );
    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("batch_together".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["output_file_id"] = serde_json::Value::String("file_together_output".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize together status"),
    )
    .expect("write together status receipt");

    let results_dir = tmp.path().join("results");
    let download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 status path"),
            "--out-dir",
            results_dir.to_str().expect("utf8 results dir"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, "together batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["artifact_count"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "output");
    assert_eq!(artifacts[0]["handle"], "file_together_output");
    assert_eq!(artifacts[0]["operation"]["provider"], "together");
    assert_eq!(
        artifacts[0]["operation"]["credential_env"],
        "TOGETHER_AI_API_KEY"
    );
    assert_eq!(
        artifacts[0]["operation"]["operation"],
        "GET https://api.together.xyz/v1/files/file_together_output/content"
    );
}

#[test]
fn models_batch_manifest_and_dry_run_groq_openai_compatible() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests_path = tmp.path().join("requests.jsonl");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &requests_path,
        r#"{"custom_id":"groq-case-1","messages":[{"role":"user","content":"grade this"}],"max_tokens":16}
"#,
    )
    .expect("write requests");

    let manifest = run(
        &[
            "models",
            "batch",
            "manifest",
            "--provider",
            "groq",
            "--model",
            "llama-3.1-8b-instant",
            "--requests",
            requests_path.to_str().expect("utf8 requests path"),
            "--out",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--json",
        ],
        &[],
    );
    assert_eq!(manifest.exit_code, 0, "harn stderr={}", manifest.stderr);

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "groq batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "groq");
    assert_eq!(job["batch"]["wire_format"], "openai");
    assert_eq!(job["batch"]["discount_percent"], 50);
    assert_eq!(job["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(job["endpoint"], "/v1/chat/completions");
    assert_eq!(job["submit"]["operation"], "POST /v1/batches");

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("groq line"),
        "groq batch line",
    );
    assert_eq!(request["custom_id"], "groq-case-1");
    assert_eq!(request["method"], "POST");
    assert_eq!(request["url"], "/v1/chat/completions");
    assert_eq!(request["body"]["model"], "llama-3.1-8b-instant");

    let receipt_path = report["receipt"].as_str().expect("receipt path");
    let submission_path = tmp.path().join("submission.json");
    let submitted = run(
        &[
            "models",
            "batch",
            "submit",
            "--receipt",
            receipt_path,
            "--out",
            submission_path.to_str().expect("utf8 submission path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(submitted.exit_code, 0, "harn stderr={}", submitted.stderr);
    let submitted_value = parse_json(&submitted.stdout, "groq batch submit");
    let submission = success_data(&submitted_value);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(submitted_job["provider"], "groq");
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "GROQ_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        "https://api.groq.com/openai/v1"
    );

    let status_path = tmp.path().join("status.json");
    let status = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission_path.to_str().expect("utf8 submission path"),
            "--out",
            status_path.to_str().expect("utf8 status path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(status.exit_code, 0, "harn stderr={}", status.stderr);

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        "groq status receipt",
    );
    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("batch_groq".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["output_file_id"] = serde_json::Value::String("file_groq_output".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize groq status"),
    )
    .expect("write groq status receipt");

    let results_dir = tmp.path().join("results");
    let download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 status path"),
            "--out-dir",
            results_dir.to_str().expect("utf8 results dir"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, "groq batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["artifact_count"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "output");
    assert_eq!(artifacts[0]["handle"], "file_groq_output");
    assert_eq!(artifacts[0]["operation"]["provider"], "groq");
    assert_eq!(artifacts[0]["operation"]["credential_env"], "GROQ_API_KEY");
    assert_eq!(
        artifacts[0]["operation"]["operation"],
        "GET https://api.groq.com/openai/v1/files/file_groq_output/content"
    );
}

#[test]
fn models_batch_prepare_xai_jsonl_and_dry_run_lifecycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &manifest_path,
        r#"{
  "schemaVersion": 1,
  "kind": "harn.model_batch_manifest",
  "producer": "test",
  "workload": "eval",
  "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 1},
  "requestCount": 1,
  "groupCount": 1,
  "groups": [
    {
      "id": "xai-fixture",
      "provider": "xai",
      "model": "grok-4",
      "workload": "eval",
      "endpoint": "provider_default",
      "tool_format": "native",
      "batch": {"api": true, "wire_format": "xai", "input_mode": "jsonl_or_inline"},
      "requests": [
        {
          "custom_id": "xai_1",
          "source_line": 1,
          "source_sha256": "fixture",
          "metadata": {},
          "request": {"messages": [{"role": "user", "content": "grade this"}], "max_tokens": 16}
        }
      ]
    }
  ],
  "warnings": []
}
"#,
    )
    .expect("write manifest");

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "xai batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "xai");
    assert_eq!(job["endpoint"], "/v1/chat/completions");
    assert_eq!(job["submit"]["operation"], "POST /v1/batches");
    assert_eq!(job["submit"]["upload"]["file"], job["request_file"]);
    assert_eq!(job["submit"]["upload"]["purpose"], serde_json::Value::Null);
    assert_eq!(
        job["submit"]["create_batch"]["input_file_id"],
        "<uploaded-file-id>"
    );

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("xai line"),
        "xai batch line",
    );
    assert_eq!(request["custom_id"], "xai_1");
    assert_eq!(request["method"], "POST");
    assert_eq!(request["url"], "/v1/chat/completions");
    assert_eq!(request["body"]["model"], "grok-4");
    assert_eq!(request["body"]["messages"][0]["content"], "grade this");

    let receipt_path = report["receipt"].as_str().expect("receipt path");
    let submission_path = tmp.path().join("submission.json");
    let submitted = run(
        &[
            "models",
            "batch",
            "submit",
            "--receipt",
            receipt_path,
            "--out",
            submission_path.to_str().expect("utf8 submission path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(submitted.exit_code, 0, "harn stderr={}", submitted.stderr);
    let submitted_value = parse_json(&submitted.stdout, "xai batch submit");
    let submission = success_data(&submitted_value);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "XAI_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        "https://api.x.ai/v1"
    );

    let status_path = tmp.path().join("status.json");
    let status = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission_path.to_str().expect("utf8 submission path"),
            "--out",
            status_path.to_str().expect("utf8 status path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(status.exit_code, 0, "harn stderr={}", status.stderr);
    let status_value = parse_json(&status.stdout, "xai batch status");
    let status_report = success_data(&status_value);
    assert_eq!(status_report["dry_run"], true);
    assert_eq!(status_report["ready_count"], 1);

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        "xai status receipt",
    );
    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("batch_xai".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["results_url"] = serde_json::Value::String(
            "https://api.x.ai/v1/batches/batch_xai/results?limit=100".to_string(),
        );
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize xai status"),
    )
    .expect("write xai status receipt");

    let results_dir = tmp.path().join("results");
    let download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 status path"),
            "--out-dir",
            results_dir.to_str().expect("utf8 results dir"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, "xai batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["ready_count"], 1);
    assert_eq!(download_report["artifact_count"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "results");
    assert_eq!(artifacts[0]["handle"], "batch_xai");
    assert_eq!(artifacts[0]["operation"]["credential_env"], "XAI_API_KEY");
    assert_eq!(
        artifacts[0]["operation"]["operation"],
        "GET https://api.x.ai/v1/batches/batch_xai/results"
    );
}

#[test]
fn models_batch_manifest_and_prepare_openai_jsonl() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let requests_path = tmp.path().join("requests.jsonl");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &requests_path,
        r#"{"custom_id":"case-1","messages":[{"role":"user","content":"grade this"}],"max_tokens":64}
{"id":"case-2","body":{"messages":[{"role":"user","content":"grade that"}],"max_tokens":32}}
"#,
    )
    .expect("write requests");

    let manifest = run(
        &[
            "models",
            "batch",
            "manifest",
            "--provider",
            "openai",
            "--model",
            "gpt-4o-mini",
            "--requests",
            requests_path.to_str().expect("utf8 requests path"),
            "--out",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--json",
        ],
        &[],
    );
    assert_eq!(manifest.exit_code, 0, "harn stderr={}", manifest.stderr);

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "batch prepare");
    let report = success_data(&prepared_value);
    assert_eq!(report["job_count"], 1);
    assert_eq!(report["request_count"], 2);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "openai");
    assert_eq!(job["batch"]["wire_format"], "openai");
    assert_eq!(job["batch"]["harn_live_adapter"]["submit"], true);
    assert_eq!(job["lifecycle"]["phase"], "prepare");
    assert_eq!(job["lifecycle"]["state"], "prepared");
    assert_eq!(job["lifecycle"]["terminal"], false);
    assert_eq!(report["lifecycle"]["state"], "prepared");
    assert_eq!(report["lifecycle"]["counts"]["prepared"], 1);
    assert_eq!(job["endpoint"], "/v1/chat/completions");
    assert_eq!(job["request_format"], "jsonl");
    assert_eq!(job["submit"]["operation"], "POST /v1/batches");

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let lines: Vec<&str> = request_text.lines().collect();
    assert_eq!(lines.len(), 2, "request_text={request_text}");
    let first = parse_json(lines[0], "first openai batch line");
    assert_eq!(first["custom_id"], "case-1");
    assert_eq!(first["method"], "POST");
    assert_eq!(first["url"], "/v1/chat/completions");
    assert_eq!(first["body"]["model"], "gpt-4o-mini");
    assert_eq!(first["body"]["messages"][0]["content"], "grade this");

    let receipt_path = report["receipt"].as_str().expect("receipt path");
    let receipt = parse_json(
        &fs::read_to_string(receipt_path).expect("read receipt"),
        "prepare receipt",
    );
    assert_eq!(receipt["kind"], "harn.model_batch_prepare_receipt");
    assert_eq!(receipt["status"], "prepared");
    assert_eq!(receipt["lifecycle"]["phase"], "prepare");
    assert_eq!(receipt["lifecycle"]["counts"]["prepared"], 1);
    assert_eq!(
        receipt["jobs"][0]["request_file_sha256"],
        job["request_file_sha256"]
    );

    let submission_path = tmp.path().join("submission.json");
    let submitted = run(
        &[
            "models",
            "batch",
            "submit",
            "--receipt",
            receipt_path,
            "--out",
            submission_path.to_str().expect("utf8 submission path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(submitted.exit_code, 0, "harn stderr={}", submitted.stderr);
    let submitted_value = parse_json(&submitted.stdout, "batch submit");
    let submission = success_data(&submitted_value);
    assert_eq!(submission["dry_run"], true);
    assert_eq!(submission["job_count"], 1);
    assert_eq!(submission["ready_count"], 1);
    assert_eq!(submission["submitted_count"], 0);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(submitted_job["lifecycle"]["phase"], "submit");
    assert_eq!(submitted_job["lifecycle"]["state"], "ready");
    assert_eq!(submitted_job["lifecycle"]["dry_run"], true);
    assert_eq!(submitted_job["lifecycle"]["cancelable"], false);
    assert_eq!(submission["lifecycle"]["state"], "dry_run");
    assert_eq!(submission["lifecycle"]["counts"]["ready"], 1);
    assert_eq!(submitted_job["provider"], "openai");
    assert_eq!(
        submitted_job["request_file_sha256"],
        job["request_file_sha256"]
    );
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "OPENAI_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["auth"],
        "OPENAI_API_KEY=<redacted>"
    );

    let submission_receipt = parse_json(
        &fs::read_to_string(&submission_path).expect("read submission receipt"),
        "submission receipt",
    );
    assert_eq!(
        submission_receipt["kind"],
        "harn.model_batch_submission_receipt"
    );
    assert_eq!(submission_receipt["status"], "dry_run");
    assert_eq!(submission_receipt["lifecycle"]["phase"], "submit");
    assert_eq!(submission_receipt["lifecycle"]["counts"]["ready"], 1);
    assert_eq!(submission_receipt["jobs"][0]["status"], "ready");

    let status_path = tmp.path().join("status.json");
    let status = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission_path.to_str().expect("utf8 submission path"),
            "--out",
            status_path.to_str().expect("utf8 status path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(status.exit_code, 0, "harn stderr={}", status.stderr);
    let status_value = parse_json(&status.stdout, "batch status");
    let status_report = success_data(&status_value);
    assert_eq!(status_report["dry_run"], true);
    assert_eq!(status_report["status"], "dry_run");
    assert_eq!(status_report["job_count"], 1);
    assert_eq!(status_report["ready_count"], 1);
    assert_eq!(status_report["completed_count"], 0);
    assert_eq!(status_report["lifecycle"]["phase"], "status");
    assert_eq!(status_report["lifecycle"]["state"], "dry_run");
    assert_eq!(status_report["lifecycle"]["counts"]["ready"], 1);
    let status_job = &status_report["jobs"].as_array().expect("status jobs")[0];
    assert_eq!(status_job["status"], "ready");
    assert_eq!(status_job["lifecycle"]["state"], "ready");
    assert_eq!(status_job["status_checked"], false);
    assert_eq!(status_job["provider"], "openai");
    assert_eq!(
        status_job["provider_batch_id"],
        serde_json::Value::String(String::new())
    );

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        "status receipt",
    );
    assert_eq!(status_receipt["kind"], "harn.model_batch_status_receipt");
    assert_eq!(status_receipt["status"], "dry_run");
    assert_eq!(status_receipt["lifecycle"]["phase"], "status");
    assert_eq!(status_receipt["lifecycle"]["counts"]["ready"], 1);
    assert_eq!(status_receipt["jobs"][0]["status"], "ready");

    status_receipt["status"] = serde_json::Value::String("running".to_string());
    status_receipt["runningCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("running".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("batch_test".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("in_progress".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize running status"),
    )
    .expect("write running status receipt");

    let cancel_path = tmp.path().join("cancel.json");
    let cancel = run(
        &[
            "models",
            "batch",
            "cancel",
            "--receipt",
            status_path.to_str().expect("utf8 status path"),
            "--out",
            cancel_path.to_str().expect("utf8 cancel path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(cancel.exit_code, 0, "harn stderr={}", cancel.stderr);
    let cancel_value = parse_json(&cancel.stdout, "batch cancel");
    let cancel_report = success_data(&cancel_value);
    assert_eq!(cancel_report["dry_run"], true);
    assert_eq!(cancel_report["status"], "dry_run");
    assert_eq!(cancel_report["job_count"], 1);
    assert_eq!(cancel_report["cancelable_count"], 1);
    assert_eq!(cancel_report["skipped_count"], 0);
    assert_eq!(cancel_report["lifecycle"]["phase"], "cancel");
    assert_eq!(cancel_report["lifecycle"]["state"], "dry_run");
    assert_eq!(cancel_report["lifecycle"]["counts"]["running"], 1);
    let cancel_job = &cancel_report["jobs"].as_array().expect("cancel jobs")[0];
    assert_eq!(cancel_job["status"], "running");
    assert_eq!(cancel_job["provider_batch_id"], "batch_test");
    assert_eq!(cancel_job["cancel_requested"], false);
    assert_eq!(
        cancel_job["cancel_operation"]["operation"],
        "POST https://api.openai.com/v1/batches/batch_test/cancel"
    );
    assert_eq!(
        cancel_job["cancel_operation"]["credential_env"],
        "OPENAI_API_KEY"
    );
    assert_eq!(
        cancel_job["cancel_operation"]["auth"],
        "OPENAI_API_KEY=<redacted>"
    );

    let cancel_receipt = parse_json(
        &fs::read_to_string(&cancel_path).expect("read cancel receipt"),
        "cancel receipt",
    );
    assert_eq!(cancel_receipt["kind"], "harn.model_batch_cancel_receipt");
    assert_eq!(cancel_receipt["status"], "dry_run");
    assert_eq!(cancel_receipt["lifecycle"]["phase"], "cancel");
    assert_eq!(
        cancel_receipt["jobs"][0]["cancel_operation"]["credential_env"],
        "OPENAI_API_KEY"
    );

    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("batch_test".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["output_file_id"] = serde_json::Value::String("file_output".to_string());
        jobs[0]["error_file_id"] = serde_json::Value::String("file_error".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize completed status"),
    )
    .expect("write completed status receipt");

    let skipped_cancel_path = tmp.path().join("cancel-completed.json");
    let skipped_cancel = run(
        &[
            "models",
            "batch",
            "cancel",
            "--receipt",
            status_path.to_str().expect("utf8 completed status path"),
            "--out",
            skipped_cancel_path
                .to_str()
                .expect("utf8 skipped cancel path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(
        skipped_cancel.exit_code, 0,
        "harn stderr={}",
        skipped_cancel.stderr
    );
    let skipped_cancel_value = parse_json(&skipped_cancel.stdout, "completed batch cancel");
    let skipped_cancel_report = success_data(&skipped_cancel_value);
    assert_eq!(skipped_cancel_report["skipped_count"], 1);
    assert_eq!(skipped_cancel_report["jobs"][0]["status"], "skipped");
    assert!(
        skipped_cancel_report["jobs"][0]["skip_reason"]
            .as_str()
            .unwrap_or("")
            .contains("terminal"),
        "skip reason={}",
        skipped_cancel_report["jobs"][0]["skip_reason"]
    );

    let results_dir = tmp.path().join("results");
    let download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 status path"),
            "--out-dir",
            results_dir.to_str().expect("utf8 results dir"),
            "--max-bytes",
            "1048576",
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, "batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["status"], "dry_run");
    assert_eq!(download_report["job_count"], 1);
    assert_eq!(download_report["ready_count"], 1);
    assert_eq!(download_report["artifact_count"], 2);
    assert_eq!(download_report["lifecycle"]["phase"], "download");
    assert_eq!(download_report["lifecycle"]["state"], "dry_run");
    assert_eq!(download_report["lifecycle"]["counts"]["ready"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    assert_eq!(download_job["status"], "ready");
    assert_eq!(download_job["lifecycle"]["state"], "ready");
    assert_eq!(download_job["lifecycle"]["result_available"], false);
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "output");
    assert_eq!(artifacts[0]["handle"], "file_output");
    assert_eq!(
        artifacts[0]["operation"]["credential_env"],
        "OPENAI_API_KEY"
    );
    assert_eq!(
        artifacts[0]["operation"]["auth"],
        "OPENAI_API_KEY=<redacted>"
    );
    assert_eq!(artifacts[1]["label"], "error");

    let results_receipt_path = results_dir.join("receipt.json");
    let results_receipt = parse_json(
        &fs::read_to_string(results_receipt_path).expect("read results receipt"),
        "results receipt",
    );
    assert_eq!(results_receipt["kind"], "harn.model_batch_results_receipt");
    assert_eq!(results_receipt["status"], "dry_run");
    assert_eq!(results_receipt["lifecycle"]["phase"], "download");
    assert_eq!(results_receipt["lifecycle"]["counts"]["ready"], 1);
    assert_eq!(results_receipt["artifactCount"], 2);

    status_receipt["status"] = serde_json::Value::String("canceled".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(0);
    status_receipt["canceledCount"] = serde_json::Value::from(1);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("canceled".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("canceled".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize canceled status"),
    )
    .expect("write canceled status receipt");

    let canceled_results_dir = tmp.path().join("canceled-results");
    let canceled_download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 canceled status path"),
            "--out-dir",
            canceled_results_dir
                .to_str()
                .expect("utf8 canceled results dir"),
            "--max-bytes",
            "1048576",
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(
        canceled_download.exit_code, 0,
        "harn stderr={}",
        canceled_download.stderr
    );
    let canceled_download_value = parse_json(&canceled_download.stdout, "canceled batch download");
    let canceled_download_report = success_data(&canceled_download_value);
    assert_eq!(canceled_download_report["dry_run"], true);
    assert_eq!(canceled_download_report["artifact_count"], 2);
    let canceled_download_job = &canceled_download_report["jobs"]
        .as_array()
        .expect("canceled download jobs")[0];
    assert_eq!(canceled_download_job["status"], "ready");
    assert_eq!(canceled_download_job["source_status"], "canceled");
    assert_eq!(canceled_download_job["artifacts"][0]["label"], "output");
}

#[test]
fn models_batch_prepare_anthropic_inline_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &manifest_path,
        r#"{
  "schemaVersion": 1,
  "kind": "harn.model_batch_manifest",
  "producer": "test",
  "workload": "judge",
  "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 1},
  "requestCount": 1,
  "groupCount": 1,
  "groups": [
    {
      "id": "anthropic-fixture",
      "provider": "anthropic",
      "model": "claude-haiku-4-5-20251001",
      "workload": "judge",
      "endpoint": "provider_default",
      "tool_format": "native",
      "batch": {"api": true, "wire_format": "anthropic_messages", "input_mode": "inline_requests"},
      "requests": [
        {
          "custom_id": "anth_1",
          "source_line": 1,
          "source_sha256": "fixture",
          "metadata": {},
          "request": {"max_tokens": 32, "messages": [{"role": "user", "content": "label this"}]}
        }
      ]
    }
  ],
  "warnings": []
}
"#,
    )
    .expect("write manifest");

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "anthropic");
    assert_eq!(job["request_format"], "json_requests");
    assert_eq!(job["endpoint"], "/v1/messages/batches");
    assert_eq!(job["submit"]["operation"], "POST /v1/messages/batches");

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_value = parse_json(
        &fs::read_to_string(request_file).expect("read request file"),
        "anthropic request body",
    );
    assert_eq!(request_value["requests"][0]["custom_id"], "anth_1");
    assert_eq!(
        request_value["requests"][0]["params"]["model"],
        "claude-haiku-4-5-20251001"
    );
    assert_eq!(
        request_value["requests"][0]["params"]["messages"][0]["content"],
        "label this"
    );
}

#[test]
fn models_batch_prepare_gemini_and_mistral_request_shapes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &manifest_path,
        r#"{
  "schemaVersion": 1,
  "kind": "harn.model_batch_manifest",
  "producer": "test",
  "workload": "corpus",
  "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 2},
  "requestCount": 2,
  "groupCount": 2,
  "groups": [
    {
      "id": "gemini-fixture",
      "provider": "gemini",
      "model": "gemini-2.5-flash-lite",
      "workload": "corpus",
      "endpoint": "provider_default",
      "tool_format": "json",
      "batch": {"api": true, "wire_format": "gemini", "input_mode": "jsonl_or_inline"},
      "requests": [
        {
          "custom_id": "gemini_1",
          "source_line": 1,
          "source_sha256": "fixture-a",
          "metadata": {},
          "request": {"contents": [{"role": "user", "parts": [{"text": "refresh"}]}]}
        }
      ]
    },
    {
      "id": "mistral-fixture",
      "provider": "mistral",
      "model": "mistral-small-2603",
      "workload": "corpus",
      "endpoint": "provider_default",
      "tool_format": "json",
      "batch": {"api": true, "wire_format": "mistral", "input_mode": "jsonl_or_inline"},
      "requests": [
        {
          "custom_id": "mistral_1",
          "source_line": 2,
          "source_sha256": "fixture-b",
          "metadata": {},
          "request": {"messages": [{"role": "user", "content": "refresh"}], "max_tokens": 16}
        }
      ]
    }
  ],
  "warnings": []
}
"#,
    )
    .expect("write manifest");

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "batch prepare");
    let report = success_data(&prepared_value);
    let jobs = report["jobs"].as_array().expect("jobs");
    assert_eq!(jobs.len(), 2);
    let gemini = jobs
        .iter()
        .find(|job| job["provider"] == "gemini")
        .expect("gemini job");
    let mistral = jobs
        .iter()
        .find(|job| job["provider"] == "mistral")
        .expect("mistral job");

    assert_eq!(gemini["endpoint"], "batchGenerateContent");
    assert_eq!(gemini["submit"]["operation"], "batches.create");
    let gemini_line = fs::read_to_string(gemini["request_file"].as_str().expect("gemini file"))
        .expect("read gemini file");
    let gemini_request = parse_json(
        gemini_line.lines().next().expect("gemini line"),
        "gemini line",
    );
    assert_eq!(gemini_request["key"], "gemini_1");
    assert_eq!(
        gemini_request["request"]["contents"][0]["parts"][0]["text"],
        "refresh"
    );
    assert!(
        gemini_request["request"]["model"].is_null(),
        "Gemini batch rows should keep model at job creation"
    );

    assert_eq!(mistral["endpoint"], "/v1/chat/completions");
    assert_eq!(mistral["submit"]["operation"], "POST /v1/batch/jobs");
    let mistral_line = fs::read_to_string(mistral["request_file"].as_str().expect("mistral file"))
        .expect("read mistral file");
    let mistral_request = parse_json(
        mistral_line.lines().next().expect("mistral line"),
        "mistral line",
    );
    assert_eq!(mistral_request["custom_id"], "mistral_1");
    assert_eq!(mistral_request["body"]["model"], "mistral-small-2603");
}

#[test]
fn models_batch_prepare_gemini_and_dry_run_lifecycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &manifest_path,
        r#"{
  "schemaVersion": 1,
  "kind": "harn.model_batch_manifest",
  "producer": "test",
  "workload": "corpus",
  "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 1},
  "requestCount": 1,
  "groupCount": 1,
  "groups": [
    {
      "id": "gemini-fixture",
      "provider": "gemini",
      "model": "gemini-2.5-flash-lite",
      "workload": "corpus",
      "endpoint": "provider_default",
      "tool_format": "json",
      "batch": {"api": true, "wire_format": "gemini", "input_mode": "jsonl_or_inline"},
      "requests": [
        {
          "custom_id": "gemini_1",
          "source_line": 1,
          "source_sha256": "fixture",
          "metadata": {},
          "request": {"contents": [{"role": "user", "parts": [{"text": "refresh"}]}]}
        }
      ]
    }
  ],
  "warnings": []
}
"#,
    )
    .expect("write manifest");

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "gemini batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "gemini");
    assert_eq!(job["endpoint"], "batchGenerateContent");
    assert_eq!(job["submit"]["operation"], "batches.create");
    assert_eq!(job["submit"]["input"]["mode"], "file_api_jsonl");
    assert_eq!(job["submit"]["input"]["file"], job["request_file"]);

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("gemini line"),
        "gemini batch line",
    );
    assert_eq!(request["key"], "gemini_1");
    assert_eq!(
        request["request"]["contents"][0]["parts"][0]["text"],
        "refresh"
    );
    assert!(
        request["request"]["model"].is_null(),
        "Gemini batch rows should keep model at job creation"
    );

    let receipt_path = report["receipt"].as_str().expect("receipt path");
    let submission_path = tmp.path().join("submission.json");
    let submitted = run(
        &[
            "models",
            "batch",
            "submit",
            "--receipt",
            receipt_path,
            "--out",
            submission_path.to_str().expect("utf8 submission path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(submitted.exit_code, 0, "harn stderr={}", submitted.stderr);
    let submitted_value = parse_json(&submitted.stdout, "gemini batch submit");
    let submission = success_data(&submitted_value);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "GEMINI_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        "https://generativelanguage.googleapis.com"
    );

    let status_path = tmp.path().join("status.json");
    let status = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission_path.to_str().expect("utf8 submission path"),
            "--out",
            status_path.to_str().expect("utf8 status path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(status.exit_code, 0, "harn stderr={}", status.stderr);
    let status_value = parse_json(&status.stdout, "gemini batch status");
    let status_report = success_data(&status_value);
    assert_eq!(status_report["dry_run"], true);
    assert_eq!(status_report["ready_count"], 1);

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        "gemini status receipt",
    );
    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] =
            serde_json::Value::String("batches/gemini-batch".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("JOB_STATE_SUCCEEDED".to_string());
        jobs[0]["responses_file"] = serde_json::Value::String("files/gemini-output".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize gemini status"),
    )
    .expect("write gemini status receipt");

    let results_dir = tmp.path().join("results");
    let download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 status path"),
            "--out-dir",
            results_dir.to_str().expect("utf8 results dir"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, "gemini batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["ready_count"], 1);
    assert_eq!(download_report["artifact_count"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "responses");
    assert_eq!(artifacts[0]["handle"], "files/gemini-output");
    assert_eq!(
        artifacts[0]["operation"]["credential_env"],
        "GEMINI_API_KEY"
    );
    assert_eq!(
        artifacts[0]["operation"]["operation"],
        "GET https://generativelanguage.googleapis.com/download/v1beta/files/gemini-output:download"
    );
}

#[test]
fn models_batch_prepare_parasail_openai_compatible_lifecycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &manifest_path,
        r#"{
  "schemaVersion": 1,
  "kind": "harn.model_batch_manifest",
  "producer": "test",
  "workload": "eval",
  "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 1},
  "requestCount": 1,
  "groupCount": 1,
  "groups": [
    {
      "id": "parasail-fixture",
      "provider": "parasail",
      "model": "openai/gpt-oss-120b",
      "workload": "eval",
      "endpoint": "provider_default",
      "tool_format": "json",
      "batch": {"api": true, "wire_format": "openai", "input_mode": "jsonl_file"},
      "requests": [
        {
          "custom_id": "parasail_1",
          "source_line": 1,
          "source_sha256": "fixture",
          "metadata": {},
          "request": {"messages": [{"role": "user", "content": "grade this"}], "max_tokens": 16}
        }
      ]
    }
  ],
  "warnings": []
}
"#,
    )
    .expect("write manifest");

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "parasail batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "parasail");
    assert_eq!(job["endpoint"], "/v1/chat/completions");
    assert_eq!(job["batch"]["wire_format"], "openai");
    assert_eq!(job["submit"]["operation"], "POST /v1/batches");
    assert_eq!(job["submit"]["upload"]["purpose"], "batch");
    assert_eq!(job["submit"]["create_batch"]["completion_window"], "24h");

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("parasail line"),
        "parasail batch line",
    );
    assert_eq!(request["custom_id"], "parasail_1");
    assert_eq!(request["method"], "POST");
    assert_eq!(request["url"], "/v1/chat/completions");
    assert_eq!(request["body"]["model"], "openai/gpt-oss-120b");
    assert_eq!(request["body"]["messages"][0]["content"], "grade this");

    let receipt_path = report["receipt"].as_str().expect("receipt path");
    let submission_path = tmp.path().join("submission.json");
    let submitted = run(
        &[
            "models",
            "batch",
            "submit",
            "--receipt",
            receipt_path,
            "--out",
            submission_path.to_str().expect("utf8 submission path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(submitted.exit_code, 0, "harn stderr={}", submitted.stderr);
    let submitted_value = parse_json(&submitted.stdout, "parasail batch submit");
    let submission = success_data(&submitted_value);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(submitted_job["provider"], "parasail");
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "PARASAIL_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        "https://api.saas.parasail.io/v1"
    );

    let status_path = tmp.path().join("status.json");
    let status = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission_path.to_str().expect("utf8 submission path"),
            "--out",
            status_path.to_str().expect("utf8 status path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(status.exit_code, 0, "harn stderr={}", status.stderr);

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        "parasail status receipt",
    );
    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("batch_parasail".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["output_file_id"] = serde_json::Value::String("file_parasail_output".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize parasail status"),
    )
    .expect("write parasail status receipt");

    let results_dir = tmp.path().join("results");
    let download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 status path"),
            "--out-dir",
            results_dir.to_str().expect("utf8 results dir"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, "parasail batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["artifact_count"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "output");
    assert_eq!(artifacts[0]["handle"], "file_parasail_output");
    assert_eq!(artifacts[0]["operation"]["provider"], "parasail");
    assert_eq!(
        artifacts[0]["operation"]["credential_env"],
        "PARASAIL_API_KEY"
    );
    assert_eq!(
        artifacts[0]["operation"]["operation"],
        "GET https://api.saas.parasail.io/v1/files/file_parasail_output/content"
    );
}

#[test]
fn models_batch_prepare_fireworks_and_dry_run_lifecycle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let manifest_path = tmp.path().join("manifest.json");
    let out_dir = tmp.path().join("prepared");
    fs::write(
        &manifest_path,
        r#"{
  "schemaVersion": 1,
  "kind": "harn.model_batch_manifest",
  "producer": "test",
  "workload": "corpus",
  "source": {"path": "fixture.jsonl", "sha256": "fixture", "row_count": 1},
  "requestCount": 1,
  "groupCount": 1,
  "groups": [
    {
      "id": "fireworks-fixture",
      "provider": "fireworks",
      "model": "accounts/fireworks/models/gpt-oss-120b",
      "workload": "corpus",
      "endpoint": "provider_default",
      "tool_format": "json",
      "batch": {"api": true, "wire_format": "fireworks", "input_mode": "jsonl_file"},
      "requests": [
        {
          "custom_id": "fireworks_1",
          "source_line": 1,
          "source_sha256": "fixture",
          "metadata": {},
          "request": {"messages": [{"role": "user", "content": "refresh"}], "max_tokens": 16}
        }
      ]
    }
  ],
  "warnings": []
}
"#,
    )
    .expect("write manifest");

    let prepared = run(
        &[
            "models",
            "batch",
            "prepare",
            "--manifest",
            manifest_path.to_str().expect("utf8 manifest path"),
            "--out-dir",
            out_dir.to_str().expect("utf8 out dir"),
            "--json",
        ],
        &[],
    );
    assert_eq!(prepared.exit_code, 0, "harn stderr={}", prepared.stderr);
    let prepared_value = parse_json(&prepared.stdout, "fireworks batch prepare");
    let report = success_data(&prepared_value);
    let job = &report["jobs"].as_array().expect("jobs")[0];
    assert_eq!(job["provider"], "fireworks");
    assert_eq!(job["endpoint"], "/v1/chat/completions");
    assert_eq!(job["batch"]["wire_format"], "fireworks");
    assert_eq!(
        job["submit"]["operation"],
        "POST /v1/accounts/{account_id}/batchInferenceJobs"
    );
    assert_eq!(
        job["submit"]["upload"]["upload_dataset"],
        "POST /v1/accounts/{account_id}/datasets/{dataset_id}:upload"
    );
    assert_eq!(job["submit"]["request_line_shape"], "{custom_id, body}");

    let request_file = job["request_file"].as_str().expect("request_file");
    let request_text = fs::read_to_string(request_file).expect("read request file");
    let request = parse_json(
        request_text.lines().next().expect("fireworks line"),
        "fireworks batch line",
    );
    assert_eq!(request["custom_id"], "fireworks_1");
    assert_eq!(request["body"]["messages"][0]["content"], "refresh");
    assert_eq!(request["body"]["max_tokens"], 16);
    assert!(
        request["body"]["model"].is_null(),
        "Fireworks batch rows keep model at job creation"
    );
    assert!(
        request["method"].is_null(),
        "Fireworks rows omit OpenAI method"
    );
    assert!(request["url"].is_null(), "Fireworks rows omit OpenAI url");

    let receipt_path = report["receipt"].as_str().expect("receipt path");
    let submission_path = tmp.path().join("submission.json");
    let submitted = run(
        &[
            "models",
            "batch",
            "submit",
            "--receipt",
            receipt_path,
            "--out",
            submission_path.to_str().expect("utf8 submission path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(submitted.exit_code, 0, "harn stderr={}", submitted.stderr);
    let submitted_value = parse_json(&submitted.stdout, "fireworks batch submit");
    let submission = success_data(&submitted_value);
    let submitted_job = &submission["jobs"].as_array().expect("submitted jobs")[0];
    assert_eq!(submitted_job["status"], "ready");
    assert_eq!(
        submitted_job["provider_operation"]["credential_env"],
        "FIREWORKS_API_KEY"
    );
    assert_eq!(
        submitted_job["provider_operation"]["base_url"],
        "https://api.fireworks.ai/v1"
    );

    let status_path = tmp.path().join("status.json");
    let status = run(
        &[
            "models",
            "batch",
            "status",
            "--submission",
            submission_path.to_str().expect("utf8 submission path"),
            "--out",
            status_path.to_str().expect("utf8 status path"),
            "--dry-run",
            "--json",
        ],
        &[],
    );
    assert_eq!(status.exit_code, 0, "harn stderr={}", status.stderr);

    let mut status_receipt = parse_json(
        &fs::read_to_string(&status_path).expect("read status receipt"),
        "fireworks status receipt",
    );
    status_receipt["status"] = serde_json::Value::String("completed".to_string());
    status_receipt["completedCount"] = serde_json::Value::from(1);
    status_receipt["readyCount"] = serde_json::Value::from(0);
    {
        let jobs = status_receipt["jobs"]
            .as_array_mut()
            .expect("mutable status jobs");
        jobs[0]["status"] = serde_json::Value::String("completed".to_string());
        jobs[0]["provider_batch_id"] = serde_json::Value::String("fw-batch".to_string());
        jobs[0]["provider_status"] = serde_json::Value::String("COMPLETED".to_string());
        jobs[0]["output_dataset_id"] = serde_json::Value::String("fw-output".to_string());
    }
    fs::write(
        &status_path,
        serde_json::to_string_pretty(&status_receipt).expect("serialize fireworks status"),
    )
    .expect("write fireworks status receipt");

    let results_dir = tmp.path().join("results");
    let download = run(
        &[
            "models",
            "batch",
            "download",
            "--status",
            status_path.to_str().expect("utf8 status path"),
            "--out-dir",
            results_dir.to_str().expect("utf8 results dir"),
            "--dry-run",
            "--json",
        ],
        &[("HARN_BATCH_FIREWORKS_ACCOUNT_ID", "acct-test")],
    );
    assert_eq!(download.exit_code, 0, "harn stderr={}", download.stderr);
    let download_value = parse_json(&download.stdout, "fireworks batch download");
    let download_report = success_data(&download_value);
    assert_eq!(download_report["dry_run"], true);
    assert_eq!(download_report["artifact_count"], 1);
    let download_job = &download_report["jobs"].as_array().expect("download jobs")[0];
    let artifacts = download_job["artifacts"]
        .as_array()
        .expect("download artifacts");
    assert_eq!(artifacts[0]["label"], "download-endpoint");
    assert_eq!(artifacts[0]["handle"], "fw-output");
    assert_eq!(artifacts[0]["operation"]["provider"], "fireworks");
    assert_eq!(
        artifacts[0]["operation"]["credential_env"],
        "FIREWORKS_API_KEY"
    );
    assert_eq!(
        artifacts[0]["operation"]["operation"],
        "GET https://api.fireworks.ai/v1/accounts/acct-test/datasets/fw-output:getDownloadEndpoint"
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
        "LoRA module format: json_with_base_model",
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
fn models_lora_inspect_human_text_omits_launch_hint_when_provider_cannot_launch_lora() {
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
            "openai",
            "--name",
            "burin-tools",
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "burin-tools -> gemma-4-e4b-it via openai",
        "catalog LoRA launch flags: no",
        "catalog LoRA rank flag: no",
        "warning: provider openai does not declare local-runtime LoRA launch flags",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
    for fragment in ["  launch:", "harn local launch", "--lora-adapter"] {
        assert!(
            !harn.stdout.contains(fragment),
            "harn stdout unexpectedly contained {fragment}: {}",
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
    let report = success_data(&harn_value);
    assert_eq!(report["base"]["selector"], "local-gemma4-e4b");
    assert_eq!(report["base"]["id"], "gemma-4-e4b-it");
    assert_eq!(report["base"]["provider"], "vllm");
    assert_eq!(report["base"]["tool_format"], "json");
    assert_eq!(report["adapter"]["name"], "burin-tools");
    assert_eq!(report["adapter"]["peft_type"], "LORA");
    assert_eq!(report["compatibility"]["base_model_match"], "suffix");
    assert_eq!(
        report["compatibility"]["provider_supports_lora_launch"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        report["compatibility"]["provider_supports_lora_max_rank"],
        serde_json::Value::Bool(true)
    );
    assert_eq!(
        report["compatibility"]["provider_lora_module_value_format"],
        "json_with_base_model"
    );
    assert_eq!(report["tool_calling"]["native_tools"], false);
    assert_eq!(report["serving"]["request_model"], "burin-tools");
    assert_eq!(report["serving"]["base_model"], "gemma-4-e4b-it");
    assert_eq!(report["serving"]["provider"], "vllm");
    assert_eq!(report["serving"]["tool_format"], "json");
    assert_eq!(
        report["serving"]["lora_module_value_format"],
        "json_with_base_model"
    );
    let serving_requirements = report["serving"]["serving_requirements"]
        .as_array()
        .expect("serving requirements");
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "parser_owner"
                && requirement["name"] == "tool_call_parser"
                && requirement["value"] == "harn_text_tool_parser"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "provider_native_tool_parser"
                && requirement["value"] == "disabled_unless_proxy_maps_to_harn_text"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    assert_eq!(report["launch"]["request_model"], "burin-tools");
    assert_eq!(report["launch"]["max_lora_rank"].as_u64(), Some(16));
    let launch = report["launch"]["harn_local_launch"]
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

#[test]
fn models_lora_inspect_manifest_contract_reports_match() {
    let contract_id = "sha256:fixture-contract";
    let adapter = write_lora_adapter_fixture_with_contract(Some(contract_id));
    let manifest = write_lora_manifest_fixture(adapter.path(), contract_id);
    let adapter_path = adapter.path().display().to_string();
    let manifest_path = manifest.display().to_string();
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
            "--manifest",
            &manifest_path,
            "--json",
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    let contract = report["contract"].as_object().expect("contract report");
    assert_eq!(contract["status"], "pass");
    assert_eq!(contract["contract_id"], contract_id);
    assert_eq!(contract["adapter_contract_id"], contract_id);
    assert_eq!(contract["base_model_match"], "exact");
    assert_eq!(contract["provider_matches"], true);
    assert_eq!(contract["tool_format_matches"], true);
    assert_eq!(contract["adapter_name_matches"], true);
    assert_eq!(
        contract["manifest"]["dataset_format"],
        "harn_text_tool_calls_json_fences"
    );
    assert!(contract["warnings"]
        .as_array()
        .expect("warnings")
        .is_empty());
}

#[test]
fn models_lora_inspect_manifest_human_text_reports_contract_route() {
    let contract_id = "sha256:fixture-contract";
    let adapter = write_lora_adapter_fixture_with_contract(Some(contract_id));
    let manifest = write_lora_manifest_fixture(adapter.path(), contract_id);
    let adapter_path = adapter.path().display().to_string();
    let manifest_path = manifest.display().to_string();
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
            "--manifest",
            &manifest_path,
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "contract: sha256:fixture-contract",
        "contract status: pass",
        "adapter contract id: sha256:fixture-contract",
        "contract route: base=exact provider=match tool_format=match adapter=match",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_inspect_require_contract_id_fails_when_adapter_omits_it() {
    let adapter = write_lora_adapter_fixture();
    let manifest = write_lora_manifest_fixture(adapter.path(), "sha256:fixture-contract");
    let adapter_path = adapter.path().display().to_string();
    let manifest_path = manifest.display().to_string();
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
            "--manifest",
            &manifest_path,
            "--require-contract-id",
            "--json",
            &adapter_path,
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(false));
    let details = &harn_value["error"]["details"];
    assert_eq!(details["contract"]["status"], "fail");
    assert_eq!(
        details["contract"]["adapter_contract_id"],
        serde_json::Value::Null
    );
    let warnings = details["warnings"].as_array().expect("warnings");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("LoRA contract missing"))),
        "warnings={warnings:?}"
    );
}

#[test]
fn models_lora_plan_human_text_includes_recipe() {
    let harn = run(
        &[
            "models",
            "lora",
            "plan",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "auto",
            "--corpus",
            "./lora-corpus",
            "--teacher",
            "dashscope/qwen3-coder-next",
            "--trainer",
            "unsloth_sft",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "LoRA plan for gemma-4-e4b-it via vllm",
        "tool format: json (requested auto)",
        "training: qlora + peft_lora",
        "trainer: unsloth_sft",
        "LoRA hparams: rank=16 alpha=32 dropout=0.05",
        "target modules: all-linear",
        "training base precision: 4bit_nf4_or_runtime_equivalent",
        "training compute precision: bf16_when_supported_else_fp16",
        "adapter precision: bf16_or_fp16_lora_weights",
        "serving base precision: same_quantization_family_as_training_or_revalidate",
        "precision gates:",
        "assistant mask: require_chat_template_generation_masks",
        "parser owner: harn_text_tool_parser",
        "split policy: train_tune_holdout_disjoint_no_eval_holdout_training",
        "template: harn_text_tool_calls_json_fences",
        "template source: Harn text tool-call parser using JSON object bodies",
        "dataset format: harn_text_tool_calls_json_fences",
        "corpus: ./lora-corpus",
        "corpus strategy: refresh (requested auto)",
        "teacher: dashscope/qwen3-coder-next via dashscope",
        "provenance manifest:",
        "hard negatives:",
        "corpus gates:",
        "trainer contract:",
        "set assistant_only_loss=true",
        "Harn remains the parser at inference",
        "use Unsloth only as the trainer backend",
        "record torch/CUDA, tokenizer class, and chat-template hash",
        "minimum trials: 5",
        "comparison baseline: same base model, provider, tool format, prompt template, and tool schemas without the adapter",
        "required metrics:",
        "Harn text parser acceptance rate",
        "require a positive paired lift before promotion; inconclusive movement stays experimental",
        "require zero contract-id drift between export manifest, adapter metadata, and served route",
        "adapter binding: runtime_lora_adapter",
        "LoRA module format: json_with_base_model",
        "serving notes:",
        "serve the adapter as a text-channel route: Harn owns tool-call parsing for this plan",
        "keep provider-native tool parsers disabled unless the proxy maps them back to Harn text tool calls",
        "promotion gates:",
        "harn models lora preflight --base local-gemma4-e4b --provider vllm --tool-format json --corpus ./lora-corpus --source-tool-format json --check",
        "harn models lora export --base local-gemma4-e4b --provider vllm --tool-format json --corpus ./lora-corpus --out ADAPTER_DATASET.jsonl --manifest ADAPTER_DATASET.manifest.json --adapter-name ADAPTER_NAME --chat-template harn_text_tool_calls_json_fences",
        "harn models lora train --base local-gemma4-e4b --provider vllm --tool-format json --dataset ADAPTER_DATASET.jsonl --export-manifest ADAPTER_DATASET.manifest.json --output-dir ADAPTER_OUTPUT_DIR --receipt-out ADAPTER_OUTPUT_DIR/train.receipt.json",
        "harn eval tool-calls --planner ADAPTER_MODEL --tool-format json --dataset ./lora-corpus",
        "harn models lora inspect --base local-gemma4-e4b --provider vllm --name ADAPTER_NAME ADAPTER_PATH_OR_REPO",
        "harn local launch local-gemma4-e4b --provider vllm --model-source gemma-4-e4b-it",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_plan_json_shape_is_stable() {
    let harn = run(
        &[
            "models",
            "lora",
            "plan",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "native",
            "--method",
            "lora",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    assert_eq!(report["base"]["id"], "gemma-4-e4b-it");
    assert_eq!(report["request"]["method"], "lora");
    assert_eq!(report["request"]["requested_tool_format"], "native");
    assert_eq!(report["request"]["effective_tool_format"], "native");
    assert_eq!(report["training"]["adapter_type"], "peft_lora");
    assert_eq!(report["training"]["trainer"], "external_sft_trainer");
    assert_eq!(report["training"]["rank"], 16);
    assert_eq!(report["training"]["alpha"], 32);
    assert_eq!(report["training"]["dropout"], 0.05);
    assert_eq!(report["training"]["quantization"], "base_model_precision");
    let target_modules = report["training"]["target_modules"]
        .as_array()
        .expect("target modules");
    assert_eq!(
        target_modules,
        &[
            serde_json::Value::from("q_proj"),
            serde_json::Value::from("k_proj"),
            serde_json::Value::from("v_proj"),
            serde_json::Value::from("o_proj"),
            serde_json::Value::from("gate_proj"),
            serde_json::Value::from("up_proj"),
            serde_json::Value::from("down_proj"),
        ]
    );
    assert_eq!(report["precision"]["schema_version"], 1);
    assert_eq!(
        report["precision"]["training_base_precision"],
        "base_model_precision"
    );
    assert_eq!(
        report["precision"]["serving_base_precision"],
        "same_base_model_precision_as_training_or_revalidate"
    );
    let precision_gates = report["precision"]["promotion_gates"]
        .as_array()
        .expect("precision promotion gates");
    assert!(
        precision_gates.iter().any(|gate| gate
            .as_str()
            .is_some_and(|text| text.contains("compute dtype"))),
        "precision gates={precision_gates:?}"
    );
    assert_eq!(
        report["training"]["contract"]["assistant_mask_policy"],
        "require_chat_template_generation_masks"
    );
    assert_eq!(
        report["training"]["contract"]["packing_policy"],
        "disabled_unless_boundary_aware_tool_pack_pairs"
    );
    assert_eq!(
        report["training"]["contract"]["tool_parser_owner"],
        "provider_tokenizer_runtime"
    );
    assert_eq!(
        report["training"]["contract"]["dataset_split_policy"],
        "train_tune_holdout_disjoint_no_eval_holdout_training"
    );
    let trainer_contract = report["training"]["trainer_contract"]
        .as_array()
        .expect("trainer contract");
    assert!(
        trainer_contract.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("assistant_only_loss=true"))),
        "trainer contract={trainer_contract:?}"
    );
    assert!(
        trainer_contract.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("messages plus a tools column"))),
        "trainer contract={trainer_contract:?}"
    );
    assert!(
        trainer_contract.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("stock TRL/PEFT backend"))),
        "trainer contract={trainer_contract:?}"
    );
    assert_eq!(report["data"]["dataset_format"], "messages_with_tool_calls");
    assert_eq!(report["request"]["requested_corpus_strategy"], "auto");
    assert_eq!(report["request"]["effective_corpus_strategy"], "audit-only");
    assert!(report["request"]["teacher"].is_null());
    assert_eq!(report["corpus_refresh"]["strategy"], "audit-only");
    assert_eq!(report["corpus_refresh"]["teacher_required"], false);
    assert_eq!(report["serving"]["adapter_binding"], "runtime_lora_adapter");
    assert_eq!(
        report["serving"]["lora_module_value_format"],
        "json_with_base_model"
    );
    assert_eq!(report["serving"]["request_model"], "ADAPTER_MODEL");
    assert_eq!(report["serving"]["adapter_name"], "ADAPTER_NAME");
    assert_eq!(report["serving"]["tool_format"], "native");
    assert_eq!(
        report["serving"]["dataset_format"],
        "messages_with_tool_calls"
    );
    let export = report["launch"]["export_command"]
        .as_array()
        .expect("export argv");
    let preflight = report["launch"]["preflight_command"]
        .as_array()
        .expect("preflight argv");
    assert!(
        preflight
            .windows(2)
            .any(|pair| pair[0] == "--tool-format" && pair[1] == "native"),
        "preflight argv={preflight:?}"
    );
    assert!(
        preflight
            .windows(2)
            .any(|pair| pair[0] == "--source-tool-format" && pair[1] == "json"),
        "preflight argv={preflight:?}"
    );
    assert!(
        export
            .windows(2)
            .any(|pair| pair[0] == "--tool-format" && pair[1] == "native"),
        "export argv={export:?}"
    );
    assert!(
        export
            .windows(2)
            .any(|pair| pair[0] == "--corpus" && pair[1] == "CORPUS_JSONL_OR_DIR"),
        "export argv={export:?}"
    );
    assert!(
        export
            .windows(2)
            .any(|pair| pair[0] == "--chat-template" && pair[1] == "gemma4_native_function_calling"),
        "export argv={export:?}"
    );
    assert!(
        export.windows(2).any(|pair| pair[0] == "--target-metadata"
            && pair[1] == "training_base_precision=base_model_precision"),
        "export argv={export:?}"
    );
    assert!(
        export.windows(2).any(|pair| pair[0] == "--target-metadata"
            && pair[1]
                == "serving_base_precision=same_base_model_precision_as_training_or_revalidate"),
        "export argv={export:?}"
    );
    let train = report["launch"]["train_command"]
        .as_array()
        .expect("train argv");
    assert!(
        train
            .windows(2)
            .any(|pair| pair[0] == "--tool-format" && pair[1] == "native"),
        "train argv={train:?}"
    );
    assert!(
        train
            .windows(2)
            .any(|pair| pair[0] == "--receipt-out"
                && pair[1] == "ADAPTER_OUTPUT_DIR/train.receipt.json"),
        "train argv={train:?}"
    );
    let serving_notes = report["serving"]["runtime_notes"]
        .as_array()
        .expect("serving runtime notes");
    assert!(
        serving_notes.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("schema-constrained or strict tool calling"))),
        "serving notes={serving_notes:?}"
    );
    assert!(
        serving_notes.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("--enable-auto-tool-choice"))),
        "serving notes={serving_notes:?}"
    );
    assert!(
        serving_notes.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("Gemma 4 native routes"))),
        "serving notes={serving_notes:?}"
    );
    assert!(
        serving_notes.iter().any(|note| note
            .as_str()
            .is_some_and(|text| text.contains("serialize Gemma 4 native-tool validation traffic"))),
        "serving notes={serving_notes:?}"
    );
    assert!(
        serving_notes.iter().any(|note| note.as_str().is_some_and(
            |text| text.contains("gemma4 tool-call parser and chat-template revision")
        )),
        "serving notes={serving_notes:?}"
    );
    let serving_requirements = report["serving"]["serving_requirements"]
        .as_array()
        .expect("serving requirements");
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "server_flag"
                && requirement["name"] == "--enable-auto-tool-choice"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "server_flag"
                && requirement["name"] == "--tool-call-parser"
                && requirement["value"] == "gemma4"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "chat_template"
                && requirement["value"] == "examples/tool_chat_template_gemma4.jinja"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "manifest_metadata"
                && requirement["name"] == "chat_template_hash"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    let warnings = report["warnings"].as_array().expect("warnings");
    assert!(
        warnings.iter().any(|warning| warning
            .as_str()
            .is_some_and(|text| text.contains("Gemma 4 native tool parsing under vLLM"))),
        "warnings={warnings:?}"
    );
    assert_eq!(report["template"]["name"], "gemma4_native_function_calling");
    assert_eq!(
        report["template"]["source"],
        "Gemma 4 tokenizer/provider native function-calling chat template"
    );
    assert_eq!(
        report["template"]["supervised_target"],
        "assistant messages with native tool_calls plus paired tool role results"
    );
    let eval = report["evaluation"]["eval_command"]
        .as_array()
        .expect("eval argv");
    assert_eq!(report["evaluation"]["minimum_trials"], 5);
    assert_eq!(
        report["evaluation"]["comparison_baseline"],
        "same base model, provider, tool format, prompt template, and tool schemas without the adapter"
    );
    let required_metrics = report["evaluation"]["required_metrics"]
        .as_array()
        .expect("required metrics");
    assert!(
        required_metrics.iter().any(|metric| metric
            .as_str()
            .is_some_and(|text| text.contains("native tool-call schema acceptance rate"))),
        "required metrics={required_metrics:?}"
    );
    let eval_gates = report["evaluation"]["gates"]
        .as_array()
        .expect("eval gates");
    assert!(
        eval_gates.iter().any(|gate| gate
            .as_str()
            .is_some_and(|text| text.contains("contract-id drift"))),
        "eval gates={eval_gates:?}"
    );
    assert!(
        eval.windows(2)
            .any(|pair| pair[0] == "--tool-format" && pair[1] == "native"),
        "eval argv={eval:?}"
    );
    let launch = report["launch"]["local_launch_command"]
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

#[test]
fn models_lora_export_check_reports_native_shape() {
    let corpus = write_lora_corpus_fixture();
    let harn = run(
        &[
            "models",
            "lora",
            "export",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "native",
            "--corpus",
            corpus.path().to_str().expect("utf8 path"),
            "--check",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "LoRA export for gemma-4-e4b-it via vllm",
        "dataset format: messages_with_tool_calls",
        "contract: sha256:",
        "contract assistant mask: require_chat_template_generation_masks",
        "contract parser owner: provider_tokenizer_runtime",
        "contract split policy: train_tune_holdout_disjoint_no_eval_holdout_training",
        "provenance defaults: split=train license=unknown",
        "required example metadata:",
        "- tool_schema_hash",
        "adapter binding: runtime_lora_adapter",
        "LoRA module format: json_with_base_model",
        "promotion minimum trials: 5",
        "promotion baseline: same base model, provider, tool format, prompt template, and tool schemas without the adapter",
        "promotion metrics:",
        "native tool-call schema acceptance rate",
        "promotion gates:",
        "require zero contract-id drift between export manifest, adapter metadata, and served route",
        "mode: check",
        "stats: records=2 emitted=1 skipped=1 tool_calls=1 tool_results=1",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_preflight_human_text_reports_readiness() {
    let corpus = write_lora_corpus_fixture();
    let config = corpus.path().join("config.yaml");
    fs::write(&config, "max_seq_length: 4096\nmin_fit_ratio: 1.0\n").expect("write config");
    let harn = run(
        &[
            "models",
            "lora",
            "preflight",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--corpus",
            corpus.path().to_str().expect("utf8 corpus path"),
            "--config",
            config.to_str().expect("utf8 config path"),
            "--min-records",
            "1",
            "--done-marker",
            "##DONE##",
            "--check",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "LoRA preflight for gemma-4-e4b-it via vllm",
        "target tool format: json",
        "expected source tool format: json",
        "export-required source tool format: json",
        "max_seq_length: 4096",
        "minimum records: 1",
        "required done marker: ##DONE##",
        "records: raw=2 trainable=1 skipped=1",
        "fit: 1/1",
        "tool calls: json=1 text=0 unknown=0 malformed_json=0",
        "declared tool formats:",
        "languages:",
        "longest examples:",
        "result: PASS",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_preflight_requires_exportable_source_for_target_format() {
    let corpus = write_lora_corpus_fixture();
    let config = corpus.path().join("config.yaml");
    fs::write(&config, "max_seq_length: 4096\nmin_fit_ratio: 1.0\n").expect("write config");
    let harn = run(
        &[
            "models",
            "lora",
            "preflight",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "text",
            "--corpus",
            corpus.path().to_str().expect("utf8 corpus path"),
            "--config",
            config.to_str().expect("utf8 config path"),
            "--source-tool-format",
            "auto",
            "--min-records",
            "1",
            "--done-marker",
            "##DONE##",
            "--check",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(false));
    let details = &harn_value["error"]["details"];
    assert_eq!(details["request"]["target_tool_format"], "text");
    assert_eq!(
        details["thresholds"]["required_export_source_tool_format"],
        "text"
    );
    let errors = details["errors"].as_array().expect("errors");
    assert!(
        errors.iter().any(|error| error
            .as_str()
            .is_some_and(|text| text.contains("text target requires text source tool calls"))),
        "errors={errors:?}"
    );
}

#[test]
fn models_lora_preflight_json_reports_failures() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let corpus = tmp.path().join("burin-tool-calling-corpus.jsonl");
    let config = tmp.path().join("config.yaml");
    fs::write(&config, "max_seq_length: 128\nmin_fit_ratio: 1.0\n").expect("write config");
    let record = serde_json::json!({
        "id": "bad-tool",
        "language": "rust",
        "task_type": "test",
        "eval_name": "bad-tool",
        "metadata": {"tool_format": "json"},
        "messages": [
            {"role": "system", "content": "Available tools: read"},
            {"role": "user", "content": "Fix it."},
            {
                "role": "assistant",
                "content": "<tool_call>\n{\"name\":\"not_live\",\"arguments\":{}}\n</tool_call>"
            }
        ]
    });
    fs::write(
        &corpus,
        serde_json::to_string(&record).expect("serialize record") + "\n",
    )
    .expect("write corpus");
    let harn = run(
        &[
            "models",
            "lora",
            "preflight",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--corpus",
            tmp.path().to_str().expect("utf8 corpus path"),
            "--config",
            config.to_str().expect("utf8 config path"),
            "--min-records",
            "1",
            "--done-marker",
            "##DONE##",
            "--check",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 1, "harn stdout={}", harn.stdout);
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(false));
    let details = &harn_value["error"]["details"];
    assert_eq!(details["stats"]["trainable_records"], 1);
    assert_eq!(
        details["stats"]["records_with_unrecognized_tools"],
        serde_json::Value::from(1)
    );
    let errors = details["errors"].as_array().expect("errors");
    assert!(
        errors.iter().any(|error| error
            .as_str()
            .is_some_and(|text| text.contains("missing required done marker"))),
        "errors={errors:?}"
    );
    assert!(
        errors.iter().any(|error| error
            .as_str()
            .is_some_and(|text| text.contains("not declared"))),
        "errors={errors:?}"
    );
}

#[test]
fn models_lora_export_json_writes_dataset_and_manifest() {
    let corpus = write_lora_corpus_fixture();
    let corpus_path = corpus.path().join("burin-tool-calling-corpus.jsonl");
    let out = corpus.path().join("structured.jsonl");
    let manifest = corpus.path().join("structured.manifest.json");
    let harn = run(
        &[
            "models",
            "lora",
            "export",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "native",
            "--corpus",
            corpus_path.to_str().expect("utf8 corpus path"),
            "--out",
            out.to_str().expect("utf8 out path"),
            "--manifest",
            manifest.to_str().expect("utf8 manifest path"),
            "--adapter-name",
            "burin-tools",
            "--chat-template",
            "gemma-4",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    assert_eq!(
        report["request"]["dataset_format"],
        "messages_with_tool_calls"
    );
    assert_eq!(report["stats"]["records"].as_u64(), Some(2));
    assert_eq!(report["stats"]["emitted"].as_u64(), Some(1));
    assert_eq!(report["stats"]["skipped"].as_u64(), Some(1));
    assert_eq!(report["stats"]["tool_calls"].as_u64(), Some(1));
    assert_eq!(report["stats"]["tool_results"].as_u64(), Some(1));
    assert_eq!(report["target"]["adapter_name"], "burin-tools");
    let contract_id = report["contract"]["id"].as_str().expect("contract id");
    assert!(
        contract_id.starts_with("sha256:"),
        "contract id={contract_id}"
    );
    assert_eq!(report["target"]["contract_id"], contract_id);
    assert_eq!(report["contract"]["base_model"], "gemma-4-e4b-it");
    assert_eq!(report["contract"]["provider"], "vllm");
    assert_eq!(report["contract"]["harn_tool_format"], "native");
    assert_eq!(
        report["contract"]["dataset_format"],
        "messages_with_tool_calls"
    );
    assert_eq!(report["contract"]["chat_template"], "gemma-4");
    assert_eq!(
        report["contract"]["training_contract"]["assistant_mask_policy"],
        "require_chat_template_generation_masks"
    );
    assert_eq!(
        report["contract"]["training_contract"]["tool_parser_owner"],
        "provider_tokenizer_runtime"
    );
    assert_eq!(
        report["contract"]["training_contract"]["dataset_split_policy"],
        "train_tune_holdout_disjoint_no_eval_holdout_training"
    );
    let required_metadata = report["contract"]["training_contract"]["required_example_metadata"]
        .as_array()
        .expect("required example metadata");
    for field in [
        "source_record_id",
        "source_transcript_id",
        "teacher_model",
        "teacher_provider",
        "target_base_model",
        "target_tool_format",
        "tool_schema_hash",
        "prompt_template_hash",
        "split",
        "license",
    ] {
        assert!(
            required_metadata.iter().any(|value| value == field),
            "required metadata missing {field}: {required_metadata:?}"
        );
    }
    assert_eq!(report["serving"]["request_model"], "burin-tools");
    assert_eq!(report["serving"]["adapter_binding"], "runtime_lora_adapter");
    assert_eq!(
        report["serving"]["lora_module_value_format"],
        "json_with_base_model"
    );
    assert_eq!(report["serving"]["contract_id"], contract_id);
    assert_eq!(report["promotion"]["minimum_trials"], 5);
    assert_eq!(
        report["promotion"]["comparison_baseline"],
        "same base model, provider, tool format, prompt template, and tool schemas without the adapter"
    );
    let promotion_eval = report["promotion"]["eval_command"]
        .as_array()
        .expect("promotion eval argv");
    assert!(
        promotion_eval
            .windows(2)
            .any(|pair| pair[0] == "--planner" && pair[1] == "burin-tools"),
        "promotion eval argv={promotion_eval:?}"
    );
    assert!(
        promotion_eval
            .windows(2)
            .any(|pair| pair[0] == "--tool-format" && pair[1] == "native"),
        "promotion eval argv={promotion_eval:?}"
    );
    assert!(out.is_file(), "exported JSONL missing");
    assert!(manifest.is_file(), "manifest missing");

    let row_text = fs::read_to_string(&out).expect("read exported JSONL");
    let row = parse_json(row_text.trim(), "export row");
    let messages = row["messages"].as_array().expect("messages array");
    assert!(
        messages.iter().any(|message| message["role"] == "assistant"
            && message["tool_calls"]
                .as_array()
                .is_some_and(|calls| calls.len() == 1)),
        "messages={messages:?}"
    );
    assert!(
        messages.iter().any(|message| message["role"] == "tool"
            && message["name"] == "read"
            && message["tool_call_id"] == "call_2_1"),
        "messages={messages:?}"
    );
    let tools = row["tools"].as_array().expect("tools array");
    assert!(
        tools.iter().any(|tool| tool["function"]["name"] == "read"),
        "tools={tools:?}"
    );
    assert_eq!(row["metadata"]["source_tool_format"], "json");
    assert_eq!(row["metadata"]["source_record_id"], "tiny-read");
    assert_eq!(row["metadata"]["source_transcript_id"], "tiny-read");
    assert_eq!(row["metadata"]["teacher_model"], "manual");
    assert_eq!(row["metadata"]["teacher_provider"], "");
    assert_eq!(row["metadata"]["target_base_model"], "gemma-4-e4b-it");
    assert_eq!(row["metadata"]["target_tool_format"], "native");
    assert_eq!(row["metadata"]["split"], "train");
    assert_eq!(row["metadata"]["license"], "unknown");
    for field in ["tool_schema_hash", "prompt_template_hash"] {
        let hash = row["metadata"][field].as_str().expect("metadata hash");
        assert!(
            hash.starts_with("sha256:") && hash.len() == "sha256:".len() + 64,
            "{field}={hash}"
        );
    }
    assert_eq!(row["metadata"]["lora_contract_id"], contract_id);
    assert_eq!(row["metadata"]["lora_target"]["contract_id"], contract_id);
    let manifest_value = parse_json(
        &fs::read_to_string(&manifest).expect("read manifest"),
        "manifest",
    );
    assert_eq!(manifest_value["contract"]["id"], contract_id);
    assert_eq!(
        manifest_value["contract"]["training_contract"]["assistant_mask_policy"],
        "require_chat_template_generation_masks"
    );
    assert_eq!(
        manifest_value["contract"]["training_contract"]["tool_parser_owner"],
        "provider_tokenizer_runtime"
    );
    assert_eq!(manifest_value["target"]["contract_id"], contract_id);
    assert_eq!(
        manifest_value["provenance"]["default_split"],
        serde_json::Value::String("train".to_string())
    );
    assert_eq!(
        manifest_value["provenance"]["default_license"],
        serde_json::Value::String("unknown".to_string())
    );
    assert!(
        manifest_value["provenance"]["required_example_metadata"]
            .as_array()
            .expect("manifest provenance fields")
            .iter()
            .any(|field| field == "tool_schema_hash"),
        "manifest provenance={:?}",
        manifest_value["provenance"]
    );
    assert_eq!(manifest_value["serving"]["request_model"], "burin-tools");
    assert_eq!(
        manifest_value["serving"]["adapter_binding"],
        "runtime_lora_adapter"
    );
    assert_eq!(
        manifest_value["serving"]["lora_module_value_format"],
        "json_with_base_model"
    );
    assert_eq!(manifest_value["serving"]["contract_id"], contract_id);
    assert_eq!(manifest_value["promotion"]["minimum_trials"], 5);
    assert!(
        manifest_value["promotion"]["gates"]
            .as_array()
            .expect("manifest promotion gates")
            .iter()
            .any(|gate| gate
                .as_str()
                .is_some_and(|text| text.contains("contract-id drift"))),
        "manifest promotion={:?}",
        manifest_value["promotion"]
    );
}

#[test]
fn models_lora_manifest_json_writes_training_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("train.jsonl");
    fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let export_manifest = tmp.path().join("export.manifest.json");
    fs::write(&export_manifest, "{}\n").expect("export manifest");
    let adapter = write_lora_adapter_fixture();
    let adapter_path = adapter.path().display().to_string();
    let out = tmp.path().join("adapter.manifest.json");
    let harn = run(
        &[
            "models",
            "lora",
            "manifest",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "json",
            "--dataset",
            dataset.to_str().expect("utf8 dataset path"),
            "--export-manifest",
            export_manifest.to_str().expect("utf8 export manifest path"),
            "--out",
            out.to_str().expect("utf8 manifest out path"),
            "--adapter-name",
            "burin-tools",
            "--adapter-path",
            &adapter_path,
            "--request-model",
            "burin-tools",
            "--trainer",
            "unsloth_sft",
            "--trainer-version",
            "unsloth-2026.7",
            "--method",
            "qlora",
            "--rank",
            "24",
            "--alpha",
            "48",
            "--training-run-id",
            "run-123",
            "--teacher",
            "dashscope/qwen3-coder-next",
            "--target-metadata",
            "lane=structured",
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    assert_eq!(report["producer"], "harn_models_lora_manifest_v1");
    assert_eq!(report["base"]["id"], "gemma-4-e4b-it");
    assert_eq!(report["target"]["adapter_name"], "burin-tools");
    assert_eq!(report["target"]["request_model"], "burin-tools");
    assert_eq!(report["target"]["harn_tool_format"], "json");
    assert_eq!(
        report["target"]["dataset_format"],
        "harn_text_tool_calls_json_fences"
    );
    assert_eq!(
        report["target"]["chat_template"],
        "harn_text_tool_calls_json_fences"
    );
    assert_eq!(report["target"]["metadata"]["lane"], "structured");
    assert_eq!(report["training"]["trainer"], "unsloth_sft");
    assert_eq!(report["training"]["method"], "qlora");
    assert_eq!(report["training"]["rank"], 24);
    assert_eq!(report["training"]["alpha"], 48);
    assert_eq!(report["training"]["run_id"], "run-123");
    assert_eq!(
        report["training"]["contract"]["tool_parser_owner"],
        "harn_text_tool_parser"
    );
    assert_eq!(
        report["training"]["precision"]["training_base_precision"],
        "4bit_nf4_or_runtime_equivalent"
    );
    assert_eq!(report["inputs"]["dataset"]["exists"], true);
    assert_eq!(report["inputs"]["dataset"]["kind"], "file");
    assert!(
        report["inputs"]["dataset"]["sha256"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64),
        "dataset input={:?}",
        report["inputs"]["dataset"]
    );
    assert_eq!(report["inputs"]["export_manifest"]["exists"], true);
    assert_eq!(
        report["inputs"]["teacher"]["id"],
        "dashscope/qwen3-coder-next"
    );
    assert_eq!(report["artifacts"]["adapter_reference"], adapter_path);
    assert_eq!(report["artifacts"]["local_path"]["kind"], "directory");
    assert!(
        report["artifacts"]["adapter_files"]
            .as_array()
            .expect("adapter files")
            .iter()
            .any(|file| file["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("adapter_config.json"))),
        "artifact files={:?}",
        report["artifacts"]["adapter_files"]
    );
    assert_eq!(report["serving"]["adapter_binding"], "runtime_lora_adapter");
    assert_eq!(
        report["serving"]["lora_module_value_format"],
        "json_with_base_model"
    );
    let serving_requirements = report["serving"]["serving_requirements"]
        .as_array()
        .expect("serving requirements");
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "parser_owner"
                && requirement["value"] == "harn_text_tool_parser"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    assert!(
        serving_requirements.iter().any(|requirement| {
            requirement["kind"] == "provider_native_tool_parser"
                && requirement["value"] == "disabled_unless_proxy_maps_to_harn_text"
                && requirement["required"] == true
        }),
        "serving requirements={serving_requirements:?}"
    );
    assert_eq!(report["promotion"]["minimum_trials"], 5);
    assert!(out.is_file(), "manifest file missing");

    let manifest_value = parse_json(
        &fs::read_to_string(&out).expect("read manifest"),
        "training manifest",
    );
    let contract_id = report["contract"]["id"].as_str().expect("contract id");
    assert_eq!(manifest_value["contract"]["id"], contract_id);
    assert_eq!(manifest_value["target"]["contract_id"], contract_id);
    assert_eq!(manifest_value["serving"]["request_model"], "burin-tools");
    assert!(
        manifest_value["serving"]["serving_requirements"]
            .as_array()
            .expect("manifest serving requirements")
            .iter()
            .any(|requirement| requirement["kind"] == "parser_owner"
                && requirement["value"] == "harn_text_tool_parser"),
        "manifest serving={:?}",
        manifest_value["serving"]
    );

    let adapter_with_contract = write_lora_adapter_fixture_with_contract(Some(contract_id));
    let inspect = run(
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
            "--manifest",
            out.to_str().expect("utf8 manifest path"),
            adapter_with_contract
                .path()
                .to_str()
                .expect("utf8 adapter path"),
            "--json",
        ],
        &[],
    );
    assert_eq!(inspect.exit_code, 0, "inspect stderr={}", inspect.stderr);
    let inspect_value = parse_json(&inspect.stdout, "inspect");
    let inspect_report = success_data(&inspect_value);
    assert_eq!(inspect_report["contract"]["status"], "pass");
    assert_eq!(inspect_report["contract"]["contract_id"], contract_id);
}

#[test]
fn models_lora_train_json_writes_dry_run_receipt() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dataset = tmp.path().join("train.jsonl");
    fs::write(&dataset, "{\"messages\":[]}\n").expect("dataset");
    let export_manifest = tmp.path().join("export.manifest.json");
    fs::write(&export_manifest, "{}\n").expect("export manifest");
    let output_dir = tmp.path().join("burin-tools");
    let receipt = tmp.path().join("train.receipt.json");
    let harn = run(
        &[
            "models",
            "lora",
            "train",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "json",
            "--dataset",
            dataset.to_str().expect("utf8 dataset path"),
            "--export-manifest",
            export_manifest.to_str().expect("utf8 export manifest path"),
            "--output-dir",
            output_dir.to_str().expect("utf8 output path"),
            "--receipt-out",
            receipt.to_str().expect("utf8 receipt path"),
            "--adapter-name",
            "burin-tools",
            "--request-model",
            "burin-tools",
            "--trainer",
            "unsloth_sft",
            "--trainer-version",
            "unsloth-2026.7",
            "--method",
            "qlora",
            "--rank",
            "24",
            "--alpha",
            "48",
            "--max-seq-length",
            "8192",
            "--target-metadata",
            "lane=structured",
            "--json",
            "--",
            "uv",
            "run",
            "python",
            "train.py",
            "config/e4b.yaml",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn lora train");
    let report = success_data(&harn_value);
    assert_eq!(report["producer"], "harn_models_lora_train_v1");
    assert_eq!(report["mode"], "dry_run");
    assert_eq!(report["base"]["id"], "gemma-4-e4b-it");
    assert_eq!(report["request"]["effective_tool_format"], "json");
    assert_eq!(
        report["request"]["dataset_format"],
        "harn_text_tool_calls_json_fences"
    );
    assert_eq!(report["target"]["adapter_name"], "burin-tools");
    assert_eq!(report["target"]["request_model"], "burin-tools");
    assert_eq!(report["target"]["metadata"]["lane"], "structured");
    assert_eq!(
        report["target"]["metadata"]["serving_tool_parser_owner"],
        "harn_text_tool_parser"
    );
    assert_eq!(
        report["target"]["metadata"]["serving_adapter_binding"],
        "runtime_lora_adapter"
    );
    assert_eq!(report["training"]["trainer"], "unsloth_sft");
    assert_eq!(report["training"]["trainer_version"], "unsloth-2026.7");
    assert_eq!(report["training"]["rank"], 24);
    assert_eq!(report["training"]["alpha"], 48);
    assert_eq!(report["training"]["max_seq_length"], 8192);
    assert_eq!(report["inputs"]["dataset"]["exists"], true);
    assert_eq!(report["inputs"]["dataset"]["kind"], "file");
    assert!(
        report["inputs"]["dataset"]["sha256"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64),
        "dataset input={:?}",
        report["inputs"]["dataset"]
    );
    assert_eq!(report["backend"]["trainer"], "unsloth_sft");
    assert_eq!(report["backend"]["execute"], false);
    assert_eq!(report["backend"]["status"], "dry_run");
    assert_eq!(
        report["backend"]["argv"]
            .as_array()
            .expect("backend argv")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>(),
        vec!["uv", "run", "python", "train.py", "config/e4b.yaml"]
    );
    assert!(
        report["post_training"]["manifest_command"]
            .as_array()
            .expect("manifest command")
            .iter()
            .any(|arg| arg == "--export-manifest"),
        "post_training={:?}",
        report["post_training"]
    );
    assert!(
        report["post_training"]["manifest_command"]
            .as_array()
            .expect("manifest command")
            .windows(2)
            .any(|pair| pair[0] == "--trainer-version" && pair[1] == "unsloth-2026.7"),
        "post_training={:?}",
        report["post_training"]
    );

    let receipt_value = parse_json(
        &fs::read_to_string(&receipt).expect("read receipt"),
        "train receipt",
    );
    assert_eq!(receipt_value["producer"], "harn_models_lora_train_v1");
    assert_eq!(receipt_value["backend"]["status"], "dry_run");
}

#[test]
fn models_lora_manifest_human_text_reports_contract() {
    let adapter = write_lora_adapter_fixture();
    let adapter_path = adapter.path().display().to_string();
    let harn = run(
        &[
            "models",
            "lora",
            "manifest",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "json",
            "--adapter-name",
            "burin-tools",
            "--adapter-path",
            &adapter_path,
            "--request-model",
            "burin-tools",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "LoRA manifest for burin-tools on gemma-4-e4b-it via vllm",
        "producer: harn_models_lora_manifest_v1",
        "tool format: json (requested json)",
        "dataset format: harn_text_tool_calls_json_fences",
        "chat template: harn_text_tool_calls_json_fences",
        "contract: sha256:",
        "request model: burin-tools",
        "trainer: external_sft_trainer",
        "training: qlora + peft_lora",
        "assistant mask: require_chat_template_generation_masks",
        "parser owner: harn_text_tool_parser",
        "adapter reference:",
        "adapter binding: runtime_lora_adapter",
        "LoRA module format: json_with_base_model",
        "promotion minimum trials: 5",
        "harn eval tool-calls --planner burin-tools --tool-format json",
        "warnings:",
        "- no --out supplied; manifest report was not written to disk",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "harn stdout missing {fragment}: {}",
            harn.stdout
        );
    }
}

#[test]
fn models_lora_export_json_structures_grouped_tool_results() {
    let corpus = write_lora_grouped_result_corpus_fixture();
    let corpus_path = corpus.path().join("burin-tool-calling-corpus.jsonl");
    let out = corpus.path().join("structured.jsonl");
    let harn = run(
        &[
            "models",
            "lora",
            "export",
            "--base",
            "local-gemma4-e4b",
            "--provider",
            "vllm",
            "--tool-format",
            "native",
            "--corpus",
            corpus_path.to_str().expect("utf8 corpus path"),
            "--out",
            out.to_str().expect("utf8 out path"),
            "--json",
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    let report = success_data(&harn_value);
    assert_eq!(report["stats"]["records"].as_u64(), Some(1));
    assert_eq!(report["stats"]["emitted"].as_u64(), Some(1));
    assert_eq!(report["stats"]["tool_calls"].as_u64(), Some(2));
    assert_eq!(report["stats"]["tool_results"].as_u64(), Some(2));

    let row_text = fs::read_to_string(&out).expect("read exported JSONL");
    let row = parse_json(row_text.trim(), "export row");
    let messages = row["messages"].as_array().expect("messages array");
    let tool_messages = messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect::<Vec<_>>();
    assert_eq!(tool_messages.len(), 2, "messages={messages:?}");
    assert_eq!(tool_messages[0]["name"], "read");
    assert_eq!(tool_messages[0]["tool_call_id"], "call_2_1");
    assert_eq!(tool_messages[0]["content"], "pub fn add() {}");
    assert_eq!(tool_messages[1]["name"], "run");
    assert_eq!(tool_messages[1]["tool_call_id"], "call_2_2");
    assert_eq!(tool_messages[1]["content"], "1 passed");
}

// ────────────────────────────────────────────────────────────────────────

fn write_lora_adapter_fixture() -> tempfile::TempDir {
    write_lora_adapter_fixture_with_contract(None)
}

fn write_lora_adapter_fixture_with_contract(contract_id: Option<&str>) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    fs::write(tmp.path().join("adapter_model.safetensors"), b"stub").expect("adapter weights");
    let mut config = serde_json::json!({
        "peft_type": "LORA",
        "base_model_name_or_path": "google/gemma-4-e4b-it",
        "task_type": "CAUSAL_LM",
        "r": 16,
        "lora_alpha": 32,
        "target_modules": ["q_proj", "v_proj"]
    });
    if let Some(contract_id) = contract_id {
        config["harn_lora_contract_id"] = serde_json::Value::String(contract_id.to_string());
    }
    fs::write(
        tmp.path().join("adapter_config.json"),
        serde_json::to_string_pretty(&config).expect("adapter config JSON"),
    )
    .expect("adapter config");
    tmp
}

fn write_lora_manifest_fixture(root: &std::path::Path, contract_id: &str) -> std::path::PathBuf {
    let path = root.join("export.manifest.json");
    let manifest = serde_json::json!({
        "exporter": "harn_models_lora_export_v1",
        "dataset_format": "harn_text_tool_calls_json_fences",
        "target": {
            "base_model": "gemma-4-e4b-it",
            "provider": "vllm",
            "adapter_name": "burin-tools",
            "harn_tool_format": "json",
            "contract_id": contract_id
        },
        "contract": {
            "schema_version": 1,
            "id": contract_id,
            "base_model": "gemma-4-e4b-it",
            "provider": "vllm",
            "harn_tool_format": "json",
            "dataset_format": "harn_text_tool_calls_json_fences",
            "chat_template": "harn_text_tool_calls_json_fences",
            "training_contract": {
                "schema_version": 1,
                "loss_scope": "assistant_tool_calls",
                "assistant_mask_policy": "require_chat_template_generation_masks",
                "packing_policy": "disabled_unless_boundary_aware_tool_pack_pairs",
                "tool_parser_owner": "harn_text_tool_parser",
                "dataset_format": "harn_text_tool_calls_json_fences",
                "dataset_split_policy": "train_tune_holdout_disjoint_no_eval_holdout_training",
                "required_example_metadata": []
            }
        },
        "serving": {
            "request_model": "burin-tools",
            "adapter_name": "burin-tools",
            "base_model": "gemma-4-e4b-it",
            "provider": "vllm",
            "adapter_binding": "runtime_lora_adapter",
            "lora_module_value_format": "json_with_base_model",
            "tool_format": "json",
            "dataset_format": "harn_text_tool_calls_json_fences",
            "contract_id": contract_id
        },
        "promotion": {
            "minimum_trials": 5,
            "gates": []
        }
    });
    fs::write(
        &path,
        serde_json::to_string_pretty(&manifest).expect("manifest JSON"),
    )
    .expect("write manifest");
    path
}

fn write_lora_corpus_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = serde_json::json!({
        "id": "tiny-read",
        "language": "rust",
        "task_type": "explain",
        "eval_name": "tiny-read",
        "model": "manual",
        "metadata": {
            "verification": "PASS"
        },
        "messages": [
            {
                "role": "system",
                "content": "Available tools: read, run\n- edit(action, path, content)\ndeclare function read(args: { path: string }): string;"
            },
            {
                "role": "user",
                "content": "Read src/lib.rs and summarize it."
            },
            {
                "role": "assistant",
                "content": "<assistant_prose>\nI will inspect the file.\n</assistant_prose>\n\n<tool_call>\n{\"name\":\"read\",\"arguments\":{\"path\":\"src/lib.rs\"}}\n</tool_call>\n"
            },
            {
                "role": "user",
                "content": "[result of read] pub fn add(a: i32, b: i32) -> i32 { a + b } [end of read result]"
            },
            {
                "role": "assistant",
                "content": "<assistant_prose>\nThe file defines an add helper.\n</assistant_prose>\n<done>##DONE##</done>"
            }
        ]
    });
    let context_row = serde_json::json!({
        "id": "source-context",
        "eval_name": "source-context",
        "source_context": {
            "notes": "metadata-only rows are not training examples"
        },
        "messages": []
    });
    fs::write(
        tmp.path().join("burin-tool-calling-corpus.jsonl"),
        serde_json::to_string(&record).expect("serialize record")
            + "\n"
            + &serde_json::to_string(&context_row).expect("serialize context row")
            + "\n",
    )
    .expect("write corpus");
    tmp
}

fn write_lora_grouped_result_corpus_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = serde_json::json!({
        "id": "grouped-results",
        "language": "rust",
        "task_type": "test",
        "eval_name": "grouped-results",
        "model": "manual",
        "metadata": {
            "tool_format": "json",
            "verification": "PASS"
        },
        "messages": [
            {
                "role": "system",
                "content": "Available tools: read, run"
            },
            {
                "role": "user",
                "content": "Inspect and test src/lib.rs."
            },
            {
                "role": "assistant",
                "content": "<tool_call>\n{\"name\":\"read\",\"arguments\":{\"path\":\"src/lib.rs\"}}\n</tool_call>\n\n<tool_call>\n{\"name\":\"run\",\"arguments\":{\"command\":\"cargo test\"}}\n</tool_call>\n"
            },
            {
                "role": "user",
                "content": "[result of read src/lib.rs]\npub fn add() {}\n[end of read result]\n\n[result of run]\n1 passed\n[end of run result]"
            }
        ]
    });
    fs::write(
        tmp.path().join("burin-tool-calling-corpus.jsonl"),
        serde_json::to_string(&record).expect("serialize record") + "\n",
    )
    .expect("write corpus");
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
