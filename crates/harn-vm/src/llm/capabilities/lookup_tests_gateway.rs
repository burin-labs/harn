use super::{clear_user_overrides, lookup};

#[test]
fn catalog_ids_inherit_upstream_model_capabilities() {
    clear_user_overrides();

    let openai = lookup("vercel_ai_gateway", "vercel/openai/gpt-5.4-nano");
    assert!(openai.native_tools);
    assert!(openai.responses_api);
    assert!(openai.reasoning_effort_supported);
    assert_eq!(openai.preferred_tool_format.as_deref(), Some("native"));
    assert!(openai.hosted_tools.is_empty());
    assert!(!openai.remote_mcp);

    let anthropic = lookup("vercel_ai_gateway", "vercel/anthropic/claude-haiku-4.5");
    assert!(anthropic.native_tools);
    assert!(anthropic.prompt_caching);
    assert_eq!(
        anthropic.cache_breakpoint_style,
        super::CacheBreakpointStyle::TopLevel
    );
    assert_eq!(anthropic.structured_output.as_deref(), Some("tool_use"));
    assert!(anthropic.responses_api);

    let gemini = lookup(
        "vercel_ai_gateway",
        "vercel/google/gemini-3.1-flash-lite-preview",
    );
    assert!(gemini.native_tools);
    assert!(gemini.vision_supported);
    assert!(gemini.pdf);
    assert!(gemini.responses_api);
    assert_eq!(gemini.preferred_tool_format.as_deref(), Some("native"));
}

#[test]
fn openrouter_explicit_cache_routes_get_block_breakpoints() {
    clear_user_overrides();
    for model in [
        "qwen/qwen3.6-plus",
        "qwen/qwen3-coder-plus",
        "qwen/qwen3-coder-flash",
        "qwen/qwen3-max",
        "qwen/qwen-plus",
    ] {
        let caps = lookup("openrouter", model);
        assert!(caps.prompt_caching, "{model} should support prompt cache");
        assert_eq!(
            caps.cache_breakpoint_style,
            super::CacheBreakpointStyle::LastBlock,
            "{model} should request explicit content-block cache breakpoints",
        );
    }

    let open_weight = lookup("openrouter", "qwen/qwen3.6-35b-a3b");
    assert!(!open_weight.prompt_caching);
    assert_eq!(
        open_weight.cache_breakpoint_style,
        super::CacheBreakpointStyle::None
    );
}
