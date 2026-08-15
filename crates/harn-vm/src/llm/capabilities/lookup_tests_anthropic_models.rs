//! Anthropic and Anthropic-wire route rows: thinking style, effort
//! controls, tool-search gating, and the OpenRouter/Bedrock mirrors that must
//! track the direct rows.

use super::lookup_tests_support::reset;
use super::model::WireDialect;
use super::*;

fn assert_openrouter_anthropic_runtime_parity(model: &str) {
    let direct = lookup("anthropic", model);
    let routed = lookup("openrouter", model);

    assert_eq!(
        routed.native_tools, direct.native_tools,
        "{model}: native tool support should match direct Anthropic"
    );
    assert_eq!(
        routed.preferred_tool_format, direct.preferred_tool_format,
        "{model}: preferred tool format should match direct Anthropic"
    );
    assert_eq!(
        routed.structured_output, direct.structured_output,
        "{model}: structured output transport should match direct Anthropic"
    );
    assert_eq!(
        routed.structured_output_mode, direct.structured_output_mode,
        "{model}: structured output mode should match direct Anthropic"
    );
    assert_eq!(
        routed.thinking_modes,
        Vec::<String>::new(),
        "{model}: OpenRouter Claude routes must not advertise direct Anthropic thinking controls"
    );
    assert!(
        !routed.reasoning_effort_supported,
        "{model}: OpenRouter Claude routes must not advertise direct Anthropic effort controls"
    );
    assert!(
        !routed.interleaved_thinking_supported,
        "{model}: OpenRouter Claude routes must not advertise interleaved thinking"
    );
    assert_eq!(
        routed.supports_assistant_prefill, direct.supports_assistant_prefill,
        "{model}: assistant prefill support should match direct Anthropic"
    );
    assert_eq!(
        routed.prompt_caching, direct.prompt_caching,
        "{model}: prompt cache support should match direct Anthropic"
    );
    assert_eq!(
        routed.prefers_xml_scaffolding, direct.prefers_xml_scaffolding,
        "{model}: XML scaffolding preference should match direct Anthropic"
    );
    assert_eq!(
        routed.prefers_markdown_scaffolding, direct.prefers_markdown_scaffolding,
        "{model}: Markdown scaffolding preference should match direct Anthropic"
    );
    assert_eq!(
        routed.prefers_role_developer, direct.prefers_role_developer,
        "{model}: developer role preference should match direct Anthropic"
    );
    assert_eq!(
        routed.prefers_xml_tools, direct.prefers_xml_tools,
        "{model}: XML tool preference should match direct Anthropic"
    );
    assert_eq!(
        routed.thinking_block_style, direct.thinking_block_style,
        "{model}: thinking block style should match direct Anthropic"
    );
    assert_eq!(
        routed.text_tool_wire_format_supported, direct.text_tool_wire_format_supported,
        "{model}: text-tool fallback support should match direct Anthropic"
    );
}

#[test]
fn anthropic_opus_47_gets_full_capabilities() {
    reset();
    let caps = lookup("anthropic", "claude-opus-4-7");
    assert!(caps.native_tools);
    assert!(caps.defer_loading);
    assert_eq!(caps.tool_search, vec!["bm25", "regex"]);
    assert!(caps.prompt_caching);
    assert_eq!(caps.thinking_modes, vec!["adaptive", "effort"]);
    assert!(caps.reasoning_effort_supported);
    assert_eq!(
        caps.reasoning_effort_levels,
        vec!["low", "medium", "high", "xhigh", "max"]
    );
    assert!(caps.interleaved_thinking_supported);
    assert!(caps.vision_supported);
    assert!(caps.audio);
    assert!(caps.pdf);
    assert!(caps.files_api_supported);
    assert_eq!(caps.max_tools, Some(10000));
    assert!(caps.prefers_xml_scaffolding);
    assert!(!caps.prefers_markdown_scaffolding);
    assert_eq!(caps.structured_output_mode, "xml_tagged");
    assert!(!caps.supports_assistant_prefill);
    assert!(!caps.prefers_role_developer);
    assert!(caps.prefers_xml_tools);
    assert_eq!(caps.thinking_block_style, "thinking_blocks");
}

#[test]
fn anthropic_sonnet_5_gets_adaptive_effort_capabilities() {
    reset();
    let caps = lookup("anthropic", "claude-sonnet-5");
    assert!(caps.native_tools);
    assert!(caps.defer_loading);
    assert_eq!(caps.tool_search, vec!["bm25", "regex"]);
    assert!(caps.prompt_caching);
    assert_eq!(caps.thinking_modes, vec!["adaptive", "effort"]);
    assert!(caps.reasoning_effort_supported);
    assert_eq!(
        caps.reasoning_effort_levels,
        vec!["low", "medium", "high", "xhigh", "max"]
    );
    assert!(caps.reasoning_disable_supported);
    assert!(!caps.reasoning_none_supported);
    assert!(caps.interleaved_thinking_supported);
    assert!(!caps.supports_assistant_prefill);
    assert_eq!(caps.thinking_block_style, "thinking_blocks");
}

#[test]
fn anthropic_fable_effort_cannot_be_disabled() {
    reset();
    for model in ["claude-fable-5", "anthropic/claude-fable-5"] {
        let caps = lookup("anthropic", model);
        assert_eq!(caps.thinking_modes, vec!["adaptive", "effort"]);
        assert!(caps.reasoning_effort_supported);
        assert_eq!(
            caps.reasoning_effort_levels,
            vec!["low", "medium", "high", "xhigh", "max"]
        );
        assert!(!caps.reasoning_disable_supported);
        assert!(!caps.supports_assistant_prefill);
    }
}

#[test]
fn anthropic_opus_46_uses_budgeted_thinking() {
    reset();
    let caps = lookup("anthropic", "claude-opus-4-6");
    assert_eq!(caps.thinking_modes, vec!["enabled"]);
    assert!(caps.interleaved_thinking_supported);
    assert!(!caps.supports_assistant_prefill);
}

#[test]
fn anthropic_opus_45_does_not_support_interleaved_thinking() {
    reset();
    let caps = lookup("anthropic", "claude-opus-4-5");
    assert_eq!(caps.thinking_modes, vec!["enabled"]);
    assert!(!caps.interleaved_thinking_supported);
    assert!(caps.supports_assistant_prefill);
}

#[test]
fn openrouter_claude_rows_track_direct_anthropic_runtime_quirks() {
    reset();
    for model in [
        "anthropic/claude-fable-5-0",
        "anthropic/claude-mythos-5-0",
        "anthropic/claude-haiku-4-5",
        "anthropic/claude-haiku-4-7",
        "anthropic/claude-sonnet-4-6",
        "anthropic/claude-sonnet-4-7",
        "anthropic/claude-sonnet-5",
        "anthropic/claude-opus-4-6",
        "anthropic/claude-opus-4-7",
    ] {
        assert_openrouter_anthropic_runtime_parity(model);
    }
}

#[test]
fn override_can_supply_anthropic_beta_features() {
    reset();
    let toml_src = r#"
[[provider.anthropic]]
model_match = "claude-custom-*"
native_tools = true
anthropic_beta_features = ["fine-grained-tool-streaming-2025-05-14"]
"#;
    set_user_overrides_toml(toml_src).unwrap();
    let caps = lookup("anthropic", "claude-custom-1");
    assert_eq!(
        caps.anthropic_beta_features,
        vec!["fine-grained-tool-streaming-2025-05-14"]
    );
    reset();
}

#[test]
fn anthropic_haiku_44_has_no_tool_search() {
    reset();
    let caps = lookup("anthropic", "claude-haiku-4-4");
    // Haiku 4.4 falls through to the `claude-*` catch-all row.
    assert!(caps.native_tools);
    assert!(caps.prompt_caching);
    assert!(!caps.defer_loading);
    assert!(caps.tool_search.is_empty());
}

#[test]
fn anthropic_haiku_45_supports_tool_search() {
    reset();
    let caps = lookup("anthropic", "claude-haiku-4-5");
    assert!(caps.defer_loading);
    assert_eq!(caps.tool_search, vec!["bm25", "regex"]);
}

#[test]
fn old_claude_gets_catchall() {
    reset();
    let caps = lookup("anthropic", "claude-opus-3-5");
    assert!(caps.native_tools);
    assert!(caps.prompt_caching);
    assert!(!caps.defer_loading);
    assert!(caps.tool_search.is_empty());
}

#[test]
fn openrouter_anthropic_claude_models_support_native_tools() {
    // Regression for #2319: OpenRouter Anthropic slugs must match the
    // Anthropic capability rules before the OpenRouter -> OpenAI family
    // chain, otherwise native-tool requests get rejected as unsupported.
    reset();
    for model in [
        "anthropic/claude-haiku-4-5",
        "anthropic/claude-haiku-4-5-20251001",
        "anthropic/claude-sonnet-4-6",
        "anthropic/claude-sonnet-4-7",
        "anthropic/claude-opus-4-7",
    ] {
        let caps = lookup("openrouter", model);
        assert!(
            caps.native_tools,
            "{model} via openrouter should report native_tools=true",
        );
        assert!(
            caps.prompt_caching,
            "{model} via openrouter should report prompt_caching=true",
        );
        assert_eq!(
            caps.cache_breakpoint_style,
            super::CacheBreakpointStyle::TopLevel,
            "{model} via openrouter should use top-level cache_control",
        );
        assert_eq!(
            caps.structured_output.as_deref(),
            Some("tool_use"),
            "{model} via openrouter should structured_output=tool_use (matches direct anthropic)",
        );
    }
}

#[test]
fn bedrock_claude_uses_anthropic_wire_capabilities() {
    reset();
    let caps = lookup("bedrock", "anthropic.claude-3-5-sonnet-20240620-v1:0");
    assert!(caps.native_tools);
    assert_eq!(caps.message_wire_format, WireDialect::Anthropic);
    assert_eq!(caps.native_tool_wire_format, "anthropic");
}

#[test]
fn openrouter_namespaced_anthropic_model() {
    reset();
    let caps = lookup("anthropic", "anthropic/claude-opus-4-7");
    assert!(caps.defer_loading);
}
