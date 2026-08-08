use super::support::{parse_json, run};

#[test]
fn models_list_human_text_renders_default_catalog_table() {
    let harn = run(&["models", "list"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert!(
        harn.stdout.starts_with("provider"),
        "stdout={}",
        harn.stdout
    );
    assert!(
        harn.stdout.contains("claude-haiku-4-5-20251001"),
        "stdout={}",
        harn.stdout
    );
}

#[test]
fn models_list_provider_filter_limits_groups() {
    let harn = run(&["models", "list", "--provider", "openai"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert!(harn.stdout.contains("openai"), "stdout={}", harn.stdout);
    assert!(!harn.stdout.contains("mock"), "stdout={}", harn.stdout);
}

#[test]
fn models_list_installed_only_is_well_formed() {
    let harn = run(&["models", "list", "--installed-only"], &[]);
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    assert!(!harn.stdout.trim().is_empty(), "stdout should not be empty");
}

#[test]
fn models_list_json_preserves_full_runtime_rows_and_price_order() {
    let harn = run(
        &[
            "models",
            "list",
            "--provider",
            "anthropic",
            "--where",
            "tier=frontier,strengths=coding",
            "--sort",
            "pricing.input",
            "--json",
        ],
        &[],
    );
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    let harn_value = parse_json(&harn.stdout, "harn");
    assert_eq!(harn_value["schemaVersion"], 1);
    assert_eq!(harn_value["query"]["provider"], "anthropic");
    assert_eq!(harn_value["query"]["sort"], "pricing.input");
    let models = harn_value["models"].as_array().expect("models array");
    assert!(!models.is_empty(), "models={models:?}");
    let mut prior = 0.0;
    for model in models {
        assert_eq!(model["provider"], "anthropic");
        assert_eq!(model["tier"], "frontier");
        assert!(
            model["strengths"]
                .as_array()
                .is_some_and(|strengths| strengths.iter().any(|value| value == "coding")),
            "strength filter admitted unexpected row: {model:?}"
        );
        assert!(model.get("tool_mode_parity").is_some());
        assert!(model.get("tool_mode_parity_notes").is_some());
        let price = model["pricing"]["input_per_mtok"]
            .as_f64()
            .expect("frontier Anthropic price");
        assert!(price >= prior, "prices must be ascending: {models:?}");
        prior = price;
    }
    assert!(models
        .iter()
        .any(|model| { model["pricing"]["cache_read_per_mtok"].is_number() }));
}

#[test]
fn models_list_filters_tool_parity_and_renders_selected_columns() {
    let harn = run(
        &[
            "models",
            "list",
            "--where",
            "tool_support.parity=native_unreliable",
            "--sort",
            "context_window",
            "--columns",
            "id,pricing.input,pricing.cache_read,tool_support.parity,tool_support.parity_notes",
        ],
        &[],
    );
    assert_eq!(harn.exit_code, 0, "harn stderr={}", harn.stderr);
    for fragment in [
        "pricing.input",
        "pricing.cache_read",
        "tool parity",
        "parity notes",
    ] {
        assert!(
            harn.stdout.contains(fragment),
            "stdout missing {fragment}: {}",
            harn.stdout
        );
    }
    assert!(
        harn.stdout.contains("native_unreliable"),
        "stdout={}",
        harn.stdout
    );
}

#[test]
fn models_list_rejects_unknown_or_ambiguous_query_input() {
    let unknown = run(
        &["models", "list", "--where", "provider_name=anthropic"],
        &[],
    );
    assert_ne!(unknown.exit_code, 0);
    assert!(
        unknown
            .stderr
            .contains("unknown where field 'provider_name'"),
        "stderr={}",
        unknown.stderr
    );

    let incompatible = run(
        &["models", "list", "--columns", "id,pricing.input", "--json"],
        &[],
    );
    assert_ne!(incompatible.exit_code, 0);
    assert!(
        incompatible
            .stderr
            .contains("--columns cannot be combined with --json"),
        "stderr={}",
        incompatible.stderr
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
    let harn = run(
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

fn test_line_keys(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|kv| kv.split('=').next().map(str::to_string))
        .collect()
}
