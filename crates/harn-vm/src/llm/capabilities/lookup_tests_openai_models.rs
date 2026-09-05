//! OpenAI-family route rows: reasoning effort surfaces, tool-search
//! gating, token-parameter quirks, and the hosts that inherit the family.

use super::lookup_tests_support::reset;
use super::model::WireDialect;
use super::*;

#[test]
fn openai_gpt_54_supports_tool_search() {
    reset();
    let caps = lookup("openai", "gpt-5.4");
    assert!(caps.defer_loading);
    assert_eq!(caps.tool_search, vec!["hosted", "client"]);
    assert_eq!(caps.json_schema.as_deref(), Some("native"));
    assert_eq!(caps.thinking_modes, vec!["effort"]);
    assert!(caps.reasoning_effort_supported);
    assert!(caps.reasoning_none_supported);
    assert!(!caps.prefers_xml_scaffolding);
    assert!(caps.prefers_markdown_scaffolding);
    assert_eq!(caps.structured_output_mode, "native_json");
    assert!(!caps.supports_assistant_prefill);
    assert!(!caps.prefers_role_developer);
    assert!(!caps.prefers_xml_tools);
    assert_eq!(caps.thinking_block_style, "reasoning_summary");
    assert_eq!(
        caps.reasoning_excluded_portable_options,
        vec![PortableOption::Temperature],
        "GPT-5.4 only fixes temperature while reasoning is active"
    );
}

#[test]
fn openai_gpt_53_has_reasoning_none_without_tool_search() {
    reset();
    let caps = lookup("openai", "gpt-5.3");
    assert!(caps.native_tools);
    assert!(!caps.defer_loading);
    assert!(caps.vision_supported);
    assert!(caps.tool_search.is_empty());
    assert_eq!(caps.thinking_modes, vec!["effort"]);
    assert!(caps.reasoning_effort_supported);
    assert!(caps.reasoning_none_supported);
}

#[test]
fn openai_original_gpt_5_has_reasoning_floor_without_none() {
    reset();
    let caps = lookup("openai", "gpt-5");
    assert!(caps.native_tools);
    assert!(!caps.defer_loading);
    assert_eq!(caps.thinking_modes, vec!["effort"]);
    assert!(caps.reasoning_effort_supported);
    assert!(!caps.reasoning_none_supported);
}

#[test]
fn openai_gpt_4o_matrix_fields_include_multimodal_support() {
    reset();
    let caps = lookup("openai", "gpt-4o");
    assert!(caps.native_tools);
    assert!(caps.vision);
    assert!(caps.audio);
    assert!(!caps.pdf);
    assert_eq!(caps.json_schema.as_deref(), Some("native"));
}

#[test]
fn openai_reasoning_models_support_effort() {
    reset();
    let caps = lookup("openai", "o3");
    assert_eq!(caps.thinking_modes, vec!["effort"]);
    assert!(caps.requires_completion_tokens);
    assert!(caps.reasoning_effort_supported);
    assert!(caps.prefers_role_developer);
    assert_eq!(caps.thinking_block_style, "reasoning_summary");
    let prefixed = lookup("openrouter", "openai/o4-mini");
    assert!(prefixed.requires_completion_tokens);
    assert!(prefixed.reasoning_effort_supported);
}

#[test]
fn openai_gpt5_requires_completion_tokens() {
    reset();
    // gpt-5.x reasoning models reject legacy `max_tokens` on
    // /v1/chat/completions and require `max_completion_tokens`.
    for model in [
        "gpt-5.5",
        "gpt-5.4",
        "gpt-5.2",
        "gpt-5.1",
        "gpt-5",
        "gpt-5-mini",
    ] {
        assert!(
            lookup("openai", model).requires_completion_tokens,
            "{model} must require max_completion_tokens"
        );
    }
    // Prefixed OpenRouter ids resolve the same way.
    assert!(lookup("openrouter", "openai/gpt-5.5").requires_completion_tokens);
}

#[test]
fn openai_gpt_5_6_exposes_exact_reasoning_efforts() {
    reset();
    for model in ["gpt-5.6", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
        let caps = lookup("openai", model);
        assert_eq!(
            caps.reasoning_effort_levels,
            vec!["none", "low", "medium", "high", "xhigh", "max"],
            "{model} reasoning effort contract"
        );
        assert!(caps.responses_api);
        assert!(caps.vision_supported);
        assert!(caps.reasoning_tools_require_responses);
        assert!(!caps.temperature_supported);
    }
}

/// GPT-6 Astra narrows the ladder again, and the generic `gpt-*`
/// `version_min = [5, 4]` row would otherwise claim `none` is available and
/// leave sampling unconstrained. Both facts were probed against the live API
/// on 2026-09-04; a wrong row here costs a provider 400 per agent turn, and
/// nothing else in the suite would catch it.
#[test]
fn openai_gpt_6_astra_narrows_effort_and_forbids_sampling() {
    reset();
    for model in ["gpt-6-astra", "gpt-6"] {
        let caps = lookup("openai", model);
        assert_eq!(
            caps.reasoning_effort_levels,
            vec!["low", "medium", "high", "xhigh"],
            "{model} reasoning effort contract"
        );
        assert!(
            !caps.reasoning_none_supported,
            "{model} rejects reasoning_effort=none"
        );
        assert!(
            !caps.temperature_supported,
            "{model} accepts only the default temperature"
        );
        assert!(!caps.top_p_supported, "{model} rejects top_p");
        assert!(caps.responses_api);
        assert!(caps.vision_supported);
        assert!(
            caps.reasoning_tools_require_responses,
            "{model} cannot carry function tools on /v1/chat/completions"
        );
    }
    // Gateway-prefixed ids resolve to the same contract.
    let routed = lookup("openrouter", "openai/gpt-6-astra");
    assert_eq!(
        routed.reasoning_effort_levels,
        vec!["low", "medium", "high", "xhigh"]
    );
    assert!(!routed.temperature_supported);
}

/// Direction control for the row above: the sibling family it sits next to
/// must keep its own, wider ladder. A row that accidentally widened Astra's
/// match glob would pass the assertions above by making every GPT model look
/// like Astra, and this is what would fail.
#[test]
fn openai_gpt_6_row_does_not_capture_gpt_5_6() {
    reset();
    let five_six = lookup("openai", "gpt-5.6-sol");
    assert!(
        five_six.reasoning_none_supported,
        "GPT-5.6 still accepts effort=none"
    );
    assert_eq!(
        five_six.reasoning_effort_levels,
        vec!["none", "low", "medium", "high", "xhigh", "max"]
    );
}

#[test]
fn openrouter_inherits_openai() {
    reset();
    let caps = lookup("openrouter", "gpt-5.4");
    assert!(caps.defer_loading);
    assert_eq!(caps.tool_search, vec!["hosted", "client"]);
    assert_eq!(caps.reasoning_wire_format.as_deref(), Some("openrouter"));
    assert!(!caps.top_k_supported);
}

#[test]
fn groq_inherits_openai_family_only() {
    reset();
    let caps = lookup("groq", "gpt-5.5-preview");
    assert!(caps.defer_loading);
}

#[test]
fn cerebras_inherits_openai_family() {
    reset();
    let caps = lookup("cerebras", "gpt-oss-120b");
    assert_eq!(caps.message_wire_format, WireDialect::OpenAiCompat);
    assert_eq!(caps.native_tool_wire_format, "openai");
    // gpt-oss uses NATIVE tool calls across cerebras/groq/together. Under
    // json/text it emits a bare {"tool","arguments"} dialect the
    // fenced-JSON parser rejects (zero parsed calls), so native is the only
    // working channel.
    assert!(caps.native_tools);
    assert_eq!(caps.preferred_tool_format.as_deref(), Some("native"));
}
