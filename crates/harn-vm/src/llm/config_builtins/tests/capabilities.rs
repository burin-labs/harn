//! `provider_capabilities` — the capability row a script sees for a route.

use super::super::catalog_builtins::provider_capabilities_builtin;
use crate::llm_config;
use crate::value::VmValue;

#[test]
fn test_provider_capabilities_exposes_tools_for_text_only_models() {
    crate::llm::capabilities::clear_user_overrides();
    let mut out = String::new();
    let args = vec![
        VmValue::String(arcstr::ArcStr::from("ollama")),
        VmValue::String(arcstr::ArcStr::from("qwen3.6:35b-a3b-coding-nvfp4")),
    ];
    let result = provider_capabilities_builtin(&args, &mut out).expect("builtin returned error");
    let dict = result.as_dict().expect("expected dict");
    // qwen3.6 on Ollama uses Harn's text tool-call wire format, not native API tools.
    // Scripts gating on `tools` should still see it as tool-capable.
    let expect_bool = |key: &str, want: bool| match dict.get(key) {
        Some(VmValue::Bool(b)) => assert_eq!(*b, want, "{key}"),
        other => panic!("expected Bool for {key}, got {other:?}"),
    };
    expect_bool("native_tools", false);
    expect_bool("text_tool_wire_format_supported", true);
    expect_bool("tools", true);
    expect_bool("prefers_markdown_scaffolding", true);
    expect_bool("prefers_xml_tools", false);
    match dict.get("structured_output_mode") {
        Some(VmValue::String(mode)) => assert_eq!(mode.as_str(), "delimited"),
        other => panic!("expected structured_output_mode string, got {other:?}"),
    }
    match dict.get("thinking_block_style") {
        Some(VmValue::String(style)) => assert_eq!(style.as_str(), "inline"),
        other => panic!("expected thinking_block_style string, got {other:?}"),
    }
}

#[test]
fn test_provider_capabilities_surfaces_batch_without_family_leakage() {
    crate::llm::capabilities::clear_user_overrides();
    let mut out = String::new();

    let direct_openai = provider_capabilities_builtin(
        &[
            VmValue::String(arcstr::ArcStr::from("openai")),
            VmValue::String(arcstr::ArcStr::from("gpt-4o")),
        ],
        &mut out,
    )
    .expect("builtin returned error");
    let direct_openai = direct_openai.as_dict().expect("expected dict");
    assert!(matches!(
        direct_openai.get("batch_api"),
        Some(VmValue::Bool(true))
    ));
    match direct_openai.get("batch_wire_format") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "openai"),
        other => panic!("expected openai batch wire format, got {other:?}"),
    }
    match direct_openai.get("batch_input_mode") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "jsonl_file"),
        other => panic!("expected jsonl_file batch input mode, got {other:?}"),
    }
    assert!(matches!(
        direct_openai.get("batch_discount_percent"),
        Some(VmValue::Int(50))
    ));
    assert!(matches!(
        direct_openai.get("batch_turnaround_hours"),
        Some(VmValue::Int(24))
    ));
    assert!(matches!(
        direct_openai.get("batch_max_requests"),
        Some(VmValue::Int(50_000))
    ));
    assert!(matches!(
        direct_openai.get("batch_max_input_bytes"),
        Some(VmValue::Int(209_715_200))
    ));
    assert!(matches!(
        direct_openai.get("batch_result_retention_days"),
        Some(VmValue::Int(30))
    ));
    match direct_openai.get("batch_result_ordering") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "custom_id_rejoin"),
        other => panic!("expected custom_id_rejoin batch ordering, got {other:?}"),
    }
    match direct_openai.get("batch_partial_failure") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "per_request"),
        other => panic!("expected per_request batch partial failure, got {other:?}"),
    }
    match direct_openai.get("batch_cancellation") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "supported"),
        other => panic!("expected supported batch cancellation, got {other:?}"),
    }
    match direct_openai.get("batch_security_notes") {
        Some(VmValue::List(notes)) => assert!(
            notes.len() >= 2,
            "expected public batch storage/security notes"
        ),
        other => panic!("expected batch security notes list, got {other:?}"),
    }
    match direct_openai.get("batch_operational_notes") {
        Some(VmValue::List(notes)) => assert!(
            notes
                .iter()
                .any(|note| note.display().contains("one model")),
            "expected OpenAI batch operational notes to preserve model-grouping policy"
        ),
        other => panic!("expected batch operational notes list, got {other:?}"),
    }
    let direct_openai_batch = direct_openai
        .get("batch")
        .and_then(VmValue::as_dict)
        .expect("expected nested batch support");
    match direct_openai_batch.get("wire_format") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "openai"),
        other => panic!("expected nested openai wire format, got {other:?}"),
    }
    assert!(matches!(
        direct_openai_batch.get("max_requests"),
        Some(VmValue::Int(50_000))
    ));
    match direct_openai_batch.get("operational_notes") {
        Some(VmValue::List(notes)) => assert!(
            notes
                .iter()
                .any(|note| note.display().contains("custom_id")),
            "expected nested batch operational notes to preserve result rejoin policy"
        ),
        other => panic!("expected nested batch operational notes list, got {other:?}"),
    }

    let openrouter_family = provider_capabilities_builtin(
        &[
            VmValue::String(arcstr::ArcStr::from("openrouter")),
            VmValue::String(arcstr::ArcStr::from("gpt-4o")),
        ],
        &mut out,
    )
    .expect("builtin returned error");
    let openrouter_family = openrouter_family.as_dict().expect("expected dict");
    assert!(matches!(
        openrouter_family.get("batch_api"),
        Some(VmValue::Bool(false))
    ));
    assert!(matches!(
        openrouter_family.get("batch_wire_format"),
        Some(VmValue::Nil)
    ));
    assert!(matches!(
        openrouter_family.get("batch_discount_percent"),
        Some(VmValue::Nil)
    ));
    assert!(matches!(
        openrouter_family.get("batch_max_requests"),
        Some(VmValue::Nil)
    ));
    assert!(matches!(
        openrouter_family.get("batch_result_ordering"),
        Some(VmValue::Nil)
    ));

    let together = provider_capabilities_builtin(
        &[
            VmValue::String(arcstr::ArcStr::from("together")),
            VmValue::String(arcstr::ArcStr::from(
                "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            )),
        ],
        &mut out,
    )
    .expect("builtin returned error");
    let together = together.as_dict().expect("expected dict");
    assert!(matches!(
        together.get("batch_api"),
        Some(VmValue::Bool(true))
    ));
    match together.get("batch_wire_format") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "openai"),
        other => panic!("expected openai-compatible batch wire format, got {other:?}"),
    }
    match together.get("batch_input_mode") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "jsonl_file"),
        other => panic!("expected jsonl_file batch input mode, got {other:?}"),
    }
    assert!(matches!(
        together.get("batch_discount_percent"),
        Some(VmValue::Int(50))
    ));
    assert!(matches!(
        together.get("batch_turnaround_hours"),
        Some(VmValue::Int(24))
    ));
    match together.get("batch_result_ordering") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "custom_id_rejoin"),
        other => panic!("expected openai-compatible batch result ordering, got {other:?}"),
    }
    match together.get("batch_cancellation") {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), "unknown"),
        other => panic!("expected unknown batch cancellation, got {other:?}"),
    }
}

#[test]
fn test_provider_capabilities_surfaces_fast_serving_tier() {
    crate::llm::capabilities::clear_user_overrides();
    let mut out = String::new();

    // Opus 4.8 advertises a usable (research-preview) fast tier.
    let opus = provider_capabilities_builtin(
        &[
            VmValue::String(arcstr::ArcStr::from("anthropic")),
            VmValue::String(arcstr::ArcStr::from("claude-opus-4-8")),
        ],
        &mut out,
    )
    .expect("builtin returned error");
    let opus = opus.as_dict().expect("expected dict");
    let expect_str = |dict: &crate::value::DictMap, key: &str, want: &str| match dict.get(key) {
        Some(VmValue::String(s)) => assert_eq!(s.as_str(), want, "{key}"),
        other => panic!("expected String for {key}, got {other:?}"),
    };
    assert!(matches!(
        opus.get("fast_tier_supported"),
        Some(VmValue::Bool(true))
    ));
    let fast = opus
        .get("fast_tier")
        .and_then(VmValue::as_dict)
        .expect("fast_tier dict present");
    expect_str(fast, "id", "fast");
    let request = fast
        .get("request")
        .and_then(VmValue::as_dict)
        .expect("fast_tier request dict present");
    expect_str(request, "param", "speed");
    expect_str(request, "beta_header", "fast-mode-2026-02-01");

    // A model with no fast tier reports false and omits the dict.
    let gpt4o = provider_capabilities_builtin(
        &[
            VmValue::String(arcstr::ArcStr::from("openai")),
            VmValue::String(arcstr::ArcStr::from("gpt-4o")),
        ],
        &mut out,
    )
    .expect("builtin returned error");
    let gpt4o = gpt4o.as_dict().expect("expected dict");
    assert!(matches!(
        gpt4o.get("fast_tier_supported"),
        Some(VmValue::Bool(false))
    ));
    assert!(gpt4o.get("fast_tier").is_none());
}

#[test]
fn test_provider_capabilities_rejects_fast_tier_without_request_knob() {
    llm_config::clear_user_overrides();
    let overlay = llm_config::parse_config_toml(concat!(
        "[providers.test_fast_gate]\n",
        "display_name = \"Test Fast Gate\"\n",
        "base_url = \"https://example.test/v1\"\n",
        "auth_style = \"none\"\n",
        "chat_endpoint = \"/chat/completions\"\n",
        "\n",
        "[models.\"test-fast-without-knob\"]\n",
        "name = \"Missing Fast Knob Test\"\n",
        "provider = \"test_fast_gate\"\n",
        "context_window = 128000\n",
        "serving_tiers = [\n",
        "  { id = \"fast\", mode = \"synchronous\", economics = \"premium\" },\n",
        "]\n",
    ))
    .expect("overlay parses");
    llm_config::set_user_overrides(Some(overlay));

    let mut out = String::new();
    let caps = provider_capabilities_builtin(
        &[
            VmValue::String(arcstr::ArcStr::from("test_fast_gate")),
            VmValue::String(arcstr::ArcStr::from("test-fast-without-knob")),
        ],
        &mut out,
    )
    .expect("builtin returned error");
    let caps = caps.as_dict().expect("expected dict");
    assert!(matches!(
        caps.get("fast_tier_supported"),
        Some(VmValue::Bool(false))
    ));
    assert!(
        caps.get("fast_tier").is_some(),
        "capability surface should still expose the malformed tier metadata"
    );

    llm_config::clear_user_overrides();
}

#[test]
fn test_provider_capabilities_exposes_prompt_format_preferences() {
    crate::llm::capabilities::clear_user_overrides();
    let mut out = String::new();
    let args = vec![
        VmValue::String(arcstr::ArcStr::from("anthropic")),
        VmValue::String(arcstr::ArcStr::from("claude-opus-4-7")),
    ];
    let result = provider_capabilities_builtin(&args, &mut out).expect("builtin returned error");
    let dict = result.as_dict().expect("expected dict");

    let expect_bool = |key: &str, want: bool| match dict.get(key) {
        Some(VmValue::Bool(b)) => assert_eq!(*b, want, "{key}"),
        other => panic!("expected Bool for {key}, got {other:?}"),
    };
    let expect_string = |key: &str, want: &str| match dict.get(key) {
        Some(VmValue::String(value)) => assert_eq!(value.as_str(), want, "{key}"),
        other => panic!("expected String for {key}, got {other:?}"),
    };

    expect_bool("prefers_xml_scaffolding", true);
    expect_bool("prefers_xml_tools", true);
    expect_bool("supports_assistant_prefill", false);
    expect_string("structured_output_mode", "xml_tagged");
    expect_string("thinking_block_style", "thinking_blocks");
}
