//! Open-weight routed families (Qwen, GLM, DeepSeek, Kimi, MiniMax) and
//! the tool-channel verdicts their rows carry across rehosting providers.

use super::lookup_tests_support::{assert_cerebras_effort_reasoning, reset};
use super::*;

#[test]
fn openrouter_qwen36_keeps_native_and_denies_ambient_upstream() {
    reset();
    for model in [
        "qwen/qwen3.6-flash",
        "qwen/qwen3.6-plus",
        "qwen/qwen3.6-35b-a3b",
    ] {
        let caps = lookup("openrouter", model);
        // The route-around must NOT downgrade the tool format: native stays on.
        assert!(caps.native_tools, "{model}: native tools");
        assert_eq!(caps.preferred_tool_format.as_deref(), Some("native"));
        // The broken Ambient upstream is denied via the data-driven denylist.
        assert_eq!(
            caps.provider_route_denylist,
            vec!["Ambient".to_string()],
            "{model}: denylist",
        );
    }
}

#[test]
fn openrouter_kimi27_code_records_tool_choice_and_sampling_limits() {
    reset();
    let caps = lookup("openrouter", "moonshotai/kimi-k2.7-code");
    assert!(caps.native_tools);
    assert!(caps.prompt_caching);
    assert!(caps.vision_supported);
    assert!(caps.video);
    // 2026-06-24 forced-format sweep flipped this route native -> text:
    // native double-escaped backslash bodies (1/5) and fenced-JSON produced
    // no parseable Harn call (0/5); heredoc text was 5/5 byte-clean.
    assert_eq!(caps.preferred_tool_format.as_deref(), Some("text"));
    assert_eq!(caps.tool_mode_parity.as_deref(), Some("native_unreliable"));
    assert_eq!(caps.thinking_modes, vec!["enabled"]);
    assert_eq!(caps.allowed_tool_choice_modes, vec!["auto", "none"]);
    assert!(!caps.temperature_supported);
    assert!(!caps.top_p_supported);
    assert!(!caps.frequency_penalty_supported);
    assert!(!caps.presence_penalty_supported);

    let prior = lookup("openrouter", "moonshotai/kimi-k2.6");
    assert!(prior.prompt_caching);
    assert!(prior.vision_supported);
    assert!(!prior.video);
    assert!(prior.allowed_tool_choice_modes.is_empty());
    assert!(prior.temperature_supported);
}

#[test]
fn qwen37_routes_record_prompt_cache_vision_and_streaming_quirks() {
    reset();
    let plus = lookup("openrouter", "qwen/qwen3.7-plus");
    assert!(plus.native_tools);
    assert!(plus.prompt_caching);
    assert!(plus.vision_supported);
    assert_eq!(plus.preferred_tool_format.as_deref(), Some("native"));
    assert_eq!(plus.thinking_modes, vec!["enabled"]);
    assert_eq!(
        plus.auto_reasoning_overrides
            .get("agent")
            .map(String::as_str),
        Some("off"),
        "Qwen tool-bearing agent turns should disable reasoning automatically",
    );

    let max = lookup("openrouter", "qwen/qwen3.7-max");
    assert!(max.native_tools);
    assert!(max.prompt_caching);
    assert!(!max.vision_supported);
    assert_eq!(max.thinking_modes, vec!["enabled"]);

    let together = lookup("together", "Qwen/Qwen3.7-Max");
    assert!(together.native_tools);
    assert!(together.prompt_caching);
    assert!(together.requires_streaming);
    assert!(!together.honors_chat_template_kwargs);

    // 2026-08-15 re-probe: GLM's native channel returned a single clean
    // `message.tool_calls` on every host, so the inherited text pin and the
    // `native_unreliable` parity are retired. The parity is compared rather
    // than pinned to an exact value: the claim being defended is that the
    // retired cross-host verdict no longer resolves here, not that the
    // no-opinion default keeps its current spelling. DeepInfra's GLM-5.2 is
    // the one surviving pin -- that deployment duplicates native tool calls --
    // and it must not widen to its siblings on the same host.
    let glm = lookup("together", "zai-org/GLM-5.1");
    assert!(glm.native_tools && glm.prompt_caching);
    assert_eq!(
        glm.auto_reasoning_overrides
            .get("agent")
            .map(String::as_str),
        Some("off"),
    );
    let or_glm = lookup("openrouter", "z-ai/glm-5.2");
    assert!(or_glm.reasoning_effort_supported);
    assert_eq!(or_glm.reasoning_effort_levels, vec!["high", "xhigh", "max"]);
    for (provider, model, fmt) in [
        ("together", "zai-org/GLM-5.1", "native"),
        ("openrouter", "z-ai/glm-5.2", "native"),
        ("deepinfra", "zai-org/GLM-5.1", "native"),
        ("deepinfra", "zai-org/GLM-5.2", "json"),
    ] {
        let caps = lookup(provider, model);
        assert_eq!(caps.preferred_tool_format.as_deref(), Some(fmt));
        let pinned = caps.tool_mode_parity.as_deref() == Some("native_unreliable");
        assert_eq!(pinned, fmt == "json", "{provider}/{model}");
    }

    let minimax = lookup("together", "MiniMaxAI/MiniMax-M2.7");
    assert!(minimax.native_tools);
    assert!(minimax.prompt_caching);
    // 2026-06-24 forced-format sweep flipped this route json -> text: heredoc
    // beat fenced-JSON on both dispatch and backslash-body fidelity at N=5.
    assert_eq!(minimax.preferred_tool_format.as_deref(), Some("text"));
    assert_eq!(
        minimax.tool_mode_parity.as_deref(),
        Some("native_unreliable")
    );
    assert!(!minimax.reasoning_text_promotable);

    let step = lookup("openrouter", "stepfun/step-3.7-flash");
    assert!(step.native_tools);
    assert!(step.prompt_caching);
    assert!(!step.reasoning_disable_supported);
    assert_eq!(step.thinking_modes, vec!["enabled"]);
}

#[test]
fn openrouter_structured_routes_cover_current_open_models() {
    reset();
    for model in [
        "deepseek/deepseek-v4-flash",
        "mistralai/devstral-small",
        "meta-llama/llama-4-scout",
        "kwaipilot/kat-coder-pro-v2",
    ] {
        let caps = lookup("openrouter", model);
        assert!(caps.native_tools, "{model} should expose native tools");
        assert_eq!(caps.structured_output.as_deref(), Some("native"));
        assert_eq!(caps.structured_output_mode, "native_json");
    }
    assert!(lookup("openrouter", "deepseek/deepseek-v4-flash").top_k_supported);
    assert!(lookup("openrouter", "meta-llama/llama-4-scout").top_k_supported);
    assert!(!lookup("openrouter", "mistralai/devstral-small").top_k_supported);
    assert!(lookup("openrouter", "google/gemma-4-26b-a4b-it").top_k_supported);
}

#[test]
fn openrouter_deepseek_v32_defaults_to_text_tools() {
    reset();
    let caps = lookup("openrouter", "deepseek/deepseek-v3.2");
    assert!(caps.native_tools);
    assert!(caps.text_tool_wire_format_supported);
    assert_eq!(caps.preferred_tool_format.as_deref(), Some("text"));
    assert_eq!(caps.tool_mode_parity.as_deref(), Some("native_unreliable"));
    assert_eq!(caps.structured_output.as_deref(), Some("native"));
    assert!(caps.prompt_caching);
    assert_eq!(
        caps.cache_breakpoint_style,
        super::CacheBreakpointStyle::LastBlock
    );

    let automated = lookup("openrouter", "deepseek/deepseek-v3");
    assert!(automated.prompt_caching);
    assert_eq!(
        automated.cache_breakpoint_style,
        super::CacheBreakpointStyle::None
    );
}

#[test]
fn openrouter_deepseek_alias_slugs_support_native_tools() {
    reset();
    for model in ["deepseek/deepseek-chat", "deepseek/deepseek-chat-v3-0324"] {
        let caps = lookup("openrouter", model);
        assert!(caps.native_tools, "{model} should expose native tools");
        assert_eq!(caps.preferred_tool_format.as_deref(), Some("native"));
        assert_eq!(caps.structured_output.as_deref(), Some("native"));
        assert!(
            caps.thinking_modes.is_empty(),
            "{model} is not a reasoning route"
        );
        assert_eq!(caps.thinking_block_style, "none");
        assert!(
            caps.top_k_supported,
            "{model} should accept top_k through OpenRouter"
        );
    }

    for model in [
        "deepseek/deepseek-chat-v3.1",
        "deepseek/deepseek-r1",
        "deepseek/deepseek-r1-0528",
    ] {
        let caps = lookup("openrouter", model);
        assert!(caps.native_tools, "{model} should expose native tools");
        assert_eq!(caps.preferred_tool_format.as_deref(), Some("native"));
        assert_eq!(caps.structured_output.as_deref(), Some("native"));
        assert_eq!(caps.thinking_modes, vec!["enabled", "effort"]);
        assert_eq!(caps.thinking_block_style, "reasoning_summary");
        assert!(
            caps.top_k_supported,
            "{model} should accept top_k through OpenRouter"
        );
    }

    assert!(!lookup("openrouter", "deepseek/deepseek-r1-distill-qwen-32b").native_tools);
}

#[test]
fn openrouter_qwen_coder_defaults_to_text_tools() {
    reset();
    let caps = lookup("openrouter", "qwen/qwen3-coder-flash");
    assert!(caps.native_tools);
    assert!(caps.text_tool_wire_format_supported);
    assert_eq!(caps.preferred_tool_format.as_deref(), Some("text"));
    assert_eq!(caps.tool_mode_parity.as_deref(), Some("native_unreliable"));
}

#[test]
fn cerebras_glm_47_supports_reasoning_none() {
    // Cerebras documents GLM 4.7's no-reasoning value as
    // reasoning_effort="none"; the older disable_reasoning knob is
    // deprecated. Keep the route on the same policy path as GPT-OSS.
    reset();
    let caps = lookup("cerebras", "zai-glm-4.7");
    assert_cerebras_effort_reasoning("zai-glm-4.7", "inline");
    assert!(caps.reasoning_none_supported);
}

#[test]
fn qwen_coder_models_do_not_claim_thinking_modes() {
    reset();
    for (provider, model) in [
        ("together", "Qwen/Qwen3-Coder-Next-FP8"),
        ("together", "Qwen/Qwen3-Coder-480B-A35B-Instruct-FP8"),
        ("openrouter", "qwen/qwen3-coder-next"),
        ("huggingface", "Qwen/Qwen3-Coder-Next"),
    ] {
        let caps = lookup(provider, model);
        assert!(caps.native_tools, "{provider}/{model}: native_tools");
        assert!(
            caps.thinking_modes.is_empty(),
            "{provider}/{model}: coder models are non-thinking routes"
        );
        assert!(
            !caps.preserve_thinking,
            "{provider}/{model}: preserve_thinking must stay off"
        );
        assert!(
            caps.thinking_disable_directive.is_none(),
            "{provider}/{model}: no /no_think shim should be needed"
        );
    }
}

#[test]
fn llamacpp_qwen_keeps_text_tool_wire_format() {
    reset();
    let caps = lookup("llamacpp", "unsloth/Qwen3.5-Coder-GGUF");
    assert_eq!(caps.server_parser, "none");
    assert!(caps.honors_chat_template_kwargs);
    assert!(!caps.native_tools);
    assert!(caps.text_tool_wire_format_supported);
    assert_eq!(
        caps.recommended_endpoint.as_deref(),
        Some("/v1/chat/completions")
    );
}
