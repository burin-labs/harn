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
        ],
        &[],
    );

    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "LoRA plan for gemma-4-e4b-it via vllm",
        "tool format: json (requested auto)",
        "training: qlora + peft_lora",
        "trainer: trl_sft_trainer",
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
        "adapter binding: runtime_lora_adapter",
        "serving notes:",
        "serve the adapter as a text-channel route: Harn owns tool-call parsing for this plan",
        "keep provider-native tool parsers disabled unless the proxy maps them back to Harn text tool calls",
        "promotion gates:",
        "harn models lora export --base local-gemma4-e4b --provider vllm --tool-format json --corpus ./lora-corpus --out ADAPTER_DATASET.jsonl --manifest ADAPTER_DATASET.manifest.json --adapter-name ADAPTER_NAME --chat-template harn_text_tool_calls_json_fences",
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
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(true));
    assert_eq!(harn_value["base"]["id"], "gemma-4-e4b-it");
    assert_eq!(harn_value["request"]["method"], "lora");
    assert_eq!(harn_value["request"]["requested_tool_format"], "native");
    assert_eq!(harn_value["request"]["effective_tool_format"], "native");
    assert_eq!(harn_value["training"]["adapter_type"], "peft_lora");
    assert_eq!(
        harn_value["training"]["quantization"],
        "base_model_precision"
    );
    let trainer_contract = harn_value["training"]["trainer_contract"]
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
    assert_eq!(
        harn_value["data"]["dataset_format"],
        "messages_with_tool_calls"
    );
    assert_eq!(harn_value["request"]["requested_corpus_strategy"], "auto");
    assert_eq!(
        harn_value["request"]["effective_corpus_strategy"],
        "audit-only"
    );
    assert!(harn_value["request"]["teacher"].is_null());
    assert_eq!(harn_value["corpus_refresh"]["strategy"], "audit-only");
    assert_eq!(harn_value["corpus_refresh"]["teacher_required"], false);
    assert_eq!(
        harn_value["serving"]["adapter_binding"],
        "runtime_lora_adapter"
    );
    assert_eq!(harn_value["serving"]["request_model"], "ADAPTER_MODEL");
    assert_eq!(harn_value["serving"]["adapter_name"], "ADAPTER_NAME");
    assert_eq!(harn_value["serving"]["tool_format"], "native");
    assert_eq!(
        harn_value["serving"]["dataset_format"],
        "messages_with_tool_calls"
    );
    let export = harn_value["launch"]["export_command"]
        .as_array()
        .expect("export argv");
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
    let serving_notes = harn_value["serving"]["runtime_notes"]
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
    assert_eq!(
        harn_value["template"]["name"],
        "gemma4_native_function_calling"
    );
    assert_eq!(
        harn_value["template"]["source"],
        "Gemma 4 tokenizer/provider native function-calling chat template"
    );
    assert_eq!(
        harn_value["template"]["supervised_target"],
        "assistant messages with native tool_calls plus paired tool role results"
    );
    let eval = harn_value["evaluation"]["eval_command"]
        .as_array()
        .expect("eval argv");
    assert!(
        eval.windows(2)
            .any(|pair| pair[0] == "--tool-format" && pair[1] == "native"),
        "eval argv={eval:?}"
    );
    let launch = harn_value["launch"]["local_launch_command"]
        .as_array()
        .expect("launch argv");
    assert!(
        launch.iter().any(|arg| arg == "--lora-adapter"),
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
        "adapter binding: runtime_lora_adapter",
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
    assert_eq!(harn_value["ok"], serde_json::Value::Bool(true));
    assert_eq!(
        harn_value["request"]["dataset_format"],
        "messages_with_tool_calls"
    );
    assert_eq!(harn_value["stats"]["records"].as_u64(), Some(2));
    assert_eq!(harn_value["stats"]["emitted"].as_u64(), Some(1));
    assert_eq!(harn_value["stats"]["skipped"].as_u64(), Some(1));
    assert_eq!(harn_value["stats"]["tool_calls"].as_u64(), Some(1));
    assert_eq!(harn_value["stats"]["tool_results"].as_u64(), Some(1));
    assert_eq!(harn_value["target"]["adapter_name"], "burin-tools");
    let contract_id = harn_value["contract"]["id"].as_str().expect("contract id");
    assert!(
        contract_id.starts_with("sha256:"),
        "contract id={contract_id}"
    );
    assert_eq!(harn_value["target"]["contract_id"], contract_id);
    assert_eq!(harn_value["contract"]["base_model"], "gemma-4-e4b-it");
    assert_eq!(harn_value["contract"]["provider"], "vllm");
    assert_eq!(harn_value["contract"]["harn_tool_format"], "native");
    assert_eq!(
        harn_value["contract"]["dataset_format"],
        "messages_with_tool_calls"
    );
    assert_eq!(harn_value["contract"]["chat_template"], "gemma-4");
    assert_eq!(harn_value["serving"]["request_model"], "burin-tools");
    assert_eq!(
        harn_value["serving"]["adapter_binding"],
        "runtime_lora_adapter"
    );
    assert_eq!(harn_value["serving"]["contract_id"], contract_id);
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
    assert_eq!(row["metadata"]["lora_contract_id"], contract_id);
    assert_eq!(row["metadata"]["lora_target"]["contract_id"], contract_id);
    let manifest_value = parse_json(
        &fs::read_to_string(&manifest).expect("read manifest"),
        "manifest",
    );
    assert_eq!(manifest_value["contract"]["id"], contract_id);
    assert_eq!(manifest_value["target"]["contract_id"], contract_id);
    assert_eq!(manifest_value["serving"]["request_model"], "burin-tools");
    assert_eq!(
        manifest_value["serving"]["adapter_binding"],
        "runtime_lora_adapter"
    );
    assert_eq!(manifest_value["serving"]["contract_id"], contract_id);
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

fn write_lora_corpus_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let record = serde_json::json!({
        "id": "tiny-read",
        "language": "rust",
        "task_type": "explain",
        "eval_name": "tiny-read",
        "model": "manual",
        "metadata": {
            "tool_format": "json",
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
