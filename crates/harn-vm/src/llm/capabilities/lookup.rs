//! Lookup and user-override facade: owns public lookup helpers.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::Deserialize;

use super::model::{Capabilities, CapabilitiesFile, ProviderLimits};
use super::overrides::{current_user_overrides, set_user_overrides as set_context_overrides};
use super::rule::lookup_with;
use super::BUILTIN_TOML;

static BUILTIN: OnceLock<CapabilitiesFile> = OnceLock::new();

pub(super) fn builtin() -> &'static CapabilitiesFile {
    BUILTIN.get_or_init(|| {
        toml::from_str::<CapabilitiesFile>(BUILTIN_TOML)
            .expect("capabilities.toml must parse at build time")
    })
}

/// The shipped (built-in) capability matrix. Public so the footgun gate in
/// [`crate::llm::capability_audit`] can audit exactly what Harn ships.
pub fn builtin_file() -> &'static CapabilitiesFile {
    builtin()
}

/// Resolve adaptive-governor limits for a provider. Thread-local user override
/// rows win over the built-in catalog, and provider ids match
/// case-insensitively like the rest of the capability matrix.
pub fn provider_limits_for(provider: &str) -> Option<ProviderLimits> {
    let key = provider.trim().to_ascii_lowercase();
    let from_map = |file: &CapabilitiesFile| -> Option<ProviderLimits> {
        file.provider_limits
            .iter()
            .find(|(name, _)| name.to_ascii_lowercase() == key)
            .map(|(_, limits)| limits.clone())
    };
    current_user_overrides()
        .as_ref()
        .and_then(from_map)
        .or_else(|| from_map(builtin()))
}

/// Provider ids that have explicit governor limit rows in the effective
/// catalog. Used by status surfaces so they do not hard-code a provider list.
pub fn provider_limit_providers() -> Vec<String> {
    let mut providers = BTreeSet::new();
    providers.extend(
        builtin()
            .provider_limits
            .keys()
            .map(|provider| provider.to_ascii_lowercase()),
    );
    if let Some(file) = current_user_overrides().as_ref() {
        providers.extend(
            file.provider_limits
                .keys()
                .map(|provider| provider.to_ascii_lowercase()),
        );
    }
    providers.into_iter().collect()
}

/// Install project-level overrides for the current thread. Usually
/// called once at CLI bootstrap after reading `harn.toml`. Passing
/// `None` clears any prior override.
pub fn set_user_overrides(file: Option<CapabilitiesFile>) {
    set_context_overrides(file);
}

/// Clear any thread-local user overrides. Used between test runs.
pub fn clear_user_overrides() {
    set_user_overrides(None);
}

/// Parse a TOML string containing the capabilities section's own shape
/// (i.e. top-level `[[provider.X]]` + optional `[provider_family]`, the
/// same layout used by the built-in `capabilities.toml`) and install as
/// the current thread's override.
pub fn set_user_overrides_toml(src: &str) -> Result<(), String> {
    set_user_overrides(Some(parse_capabilities_toml(src)?));
    Ok(())
}

/// Parse a capabilities TOML document (the same layout used by the built-in
/// `capabilities.toml`) without installing it anywhere, for callers that
/// thread an explicit capability overlay instead of mutating thread state
/// (e.g. `harn provider catalog export --capabilities-overlay`).
pub fn parse_capabilities_toml(src: &str) -> Result<CapabilitiesFile, String> {
    toml::from_str(src).map_err(|e| e.to_string())
}

/// Extract the `[capabilities]` section from a full `harn.toml` source
/// and install it as the current thread's override. The schema inside
/// that section mirrors `CapabilitiesFile` but with every key prefixed
/// by `capabilities.`:
///
/// ```toml
/// [[capabilities.provider.my-proxy]]
/// model_match = "*"
/// native_tools = true
/// tool_search = ["hosted"]
/// ```
pub fn set_user_overrides_from_manifest_toml(src: &str) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Manifest {
        #[serde(default)]
        capabilities: Option<CapabilitiesFile>,
    }
    let parsed: Manifest = toml::from_str(src).map_err(|e| e.to_string())?;
    set_user_overrides(parsed.capabilities);
    Ok(())
}

/// Look up effective capabilities for a `(provider, model)` pair.
/// Walks the provider_family chain until it finds a rule list that
/// matches. Within any one provider's rule list, user overrides are
/// consulted before the built-in rules. The first matching rule wins —
/// later rules (and later layers in the family chain) are ignored —
/// unless it sets `extends = true`, in which case it contributes only the
/// fields it explicitly sets and resolution continues to later matching
/// rules (and ultimately provider / built-in defaults) to fill the rest.
pub fn lookup(provider: &str, model: &str) -> Capabilities {
    let user = current_user_overrides();
    lookup_with_user_overrides(provider, model, user.as_ref())
}

pub fn lookup_with_user_overrides(
    provider: &str,
    model: &str,
    user_overrides: Option<&CapabilitiesFile>,
) -> Capabilities {
    let model = crate::llm_config::capability_model_id(provider, model);
    finish_lookup(
        provider,
        lookup_with(provider, &model, builtin(), user_overrides),
    )
}

pub fn lookup_with_base_file(provider: &str, model: &str, base: &CapabilitiesFile) -> Capabilities {
    let model = crate::llm_config::capability_model_id(provider, model);
    finish_lookup(provider, lookup_with(provider, &model, base, None))
}

fn finish_lookup(provider: &str, mut caps: Capabilities) -> Capabilities {
    if provider != "openai"
        && provider != "mock"
        && !crate::llm_config::provider_has_feature(provider, "responses_api")
    {
        caps.responses_api = false;
        caps.chat_completions_unsupported = false;
    }
    if provider != "openai" && provider != "mock" {
        caps.hosted_tools.clear();
        caps.remote_mcp = false;
        caps.conversation_state = false;
        caps.compaction = false;
        caps.background_mode = false;
        caps.tool_approval_policy = None;
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::super::model::WireDialect;
    use super::*;

    fn reset() {
        clear_user_overrides();
    }

    #[test]
    fn reasoning_text_promotion_is_explicit_opt_in() {
        reset();
        assert!(!lookup("openai", "synthetic-default").reasoning_text_promotable);

        set_user_overrides_toml(concat!(
            "[[provider.openai]]\n",
            "model_match = \"synthetic-promotable\"\n",
            "reasoning_text_promotable = true\n",
        ))
        .expect("capability override");
        assert!(lookup("openai", "synthetic-promotable").reasoning_text_promotable);
        reset();
    }

    fn assert_cerebras_effort_reasoning(model: &str, thinking_block_style: &str) {
        let caps = lookup("cerebras", model);
        assert_eq!(caps.thinking_modes, vec!["effort"]);
        assert!(caps.reasoning_effort_supported);
        // tool_format is NOT asserted here: cerebras gpt-oss and zai-glm have
        // different defaults (gpt-oss harmonized to `json`, glm stays
        // `native`), and this shared helper is about reasoning-effort
        // behavior. Tool-format resolution is asserted in the dedicated
        // harmonization tests.
        assert_eq!(caps.structured_output.as_deref(), Some("native"));
        assert_eq!(caps.structured_output_mode, "native_json");
        assert_eq!(caps.thinking_block_style, thinking_block_style);
    }

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
    fn provider_route_denylist_defaults_empty_for_unmarked_rows() {
        reset();
        let caps = lookup("anthropic", "claude-opus-4-7");
        assert!(caps.provider_route_denylist.is_empty());
    }

    #[test]
    fn strict_openai_compat_rows_require_tool_result_adjacency() {
        reset();
        assert!(lookup("moonshot", "moonshot/kimi-k2.6").requires_tool_result_adjacency);
        assert!(lookup("moonshot", "moonshot/kimi-k2.7-code").requires_tool_result_adjacency);
        assert!(lookup("minimax", "MiniMax-M2").requires_tool_result_adjacency);
        assert!(lookup("minimax", "MiniMax-M2.7").requires_tool_result_adjacency);
        assert!(!lookup("openai", "gpt-4o").requires_tool_result_adjacency);
    }

    #[test]
    fn moonshot_kimi_gates_temperature_and_top_p_but_other_hosts_do_not() {
        reset();
        // Moonshot's API pins temperature/top_p to one value and 400s on any
        // other; the general `*kimi*` rule strips both so callers never hit it.
        for id in [
            "moonshot/kimi-k2.5",
            "moonshot/kimi-k2.6",
            "moonshot/kimi-k2.7-code",
        ] {
            let caps = lookup("moonshot", id);
            assert!(
                !caps.temperature_supported,
                "{id} temperature should be gated"
            );
            assert!(!caps.top_p_supported, "{id} top_p should be gated");
        }
        // The same weights on Fireworks accept both — the restriction is a
        // Moonshot serving-API policy, not a model limit, so it stays scoped to
        // the moonshot route.
        let fw = lookup("fireworks", "accounts/fireworks/models/kimi-k2p6");
        assert!(fw.temperature_supported);
        assert!(fw.top_p_supported);
    }

    #[test]
    fn fireworks_gpt_oss_disables_parallel_tool_call_history() {
        reset();
        assert!(
            !lookup("fireworks", "accounts/fireworks/models/gpt-oss-120b")
                .supports_parallel_tool_calls
        );
        assert!(lookup("openai", "gpt-4o").supports_parallel_tool_calls);
    }

    #[test]
    fn cerebras_tools_exclude_response_format() {
        reset();
        assert!(lookup("cerebras", "gpt-oss-120b").tools_exclude_response_format);
        assert!(lookup("cerebras", "zai-glm-4.7").tools_exclude_response_format);
        assert!(!lookup("openai", "gpt-4o").tools_exclude_response_format);
    }

    #[test]
    fn serving_precision_seeds_known_gpt_oss_verdicts() {
        reset();
        // Full-precision routes verified during the 2026-06 meter effort.
        assert_eq!(
            lookup("fireworks", "accounts/fireworks/models/gpt-oss-120b").serving_precision,
            "trusted"
        );
        assert_eq!(
            lookup("openrouter", "openai/gpt-oss-120b").serving_precision,
            "trusted"
        );
        // SambaNova serves gpt-oss quantized (proven 0/5 vs reference 3/3).
        assert_eq!(
            lookup("sambanova", "gpt-oss-120b").serving_precision,
            "degraded"
        );
        // Cerebras is full precision but rate-throttled to unusable timing.
        assert_eq!(
            lookup("cerebras", "gpt-oss-120b").serving_precision,
            "throttled"
        );
    }

    #[test]
    fn serving_precision_defaults_unverified_for_unmarked_rows() {
        reset();
        // A route with no serving_precision verdict resolves to "unverified",
        // never an empty string, so callers can branch on a stable enum.
        assert_eq!(
            lookup("anthropic", "claude-opus-4-7").serving_precision,
            "unverified"
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
    fn gemini_thinking_budget_quirks_are_declared_in_matrix() {
        reset();
        // Flash: 24576 ceiling, can disable thinking.
        let flash = lookup("gemini", "gemini-2.5-flash");
        assert_eq!(flash.max_thinking_budget, Some(24_576));
        assert!(flash.reasoning_disable_supported);
        assert!(flash.thinking_modes.iter().any(|m| m == "effort"));
        // Pro: 32768 ceiling, cannot disable thinking.
        let pro = lookup("gemini", "gemini-2.5-pro");
        assert_eq!(pro.max_thinking_budget, Some(32_768));
        assert!(!pro.reasoning_disable_supported);
        assert!(pro.thinking_modes.iter().any(|m| m == "effort"));
        // The `models/` REST resource name resolves the same.
        let flash_resource = lookup("gemini", "models/gemini-2.5-flash");
        assert_eq!(flash_resource.max_thinking_budget, Some(24_576));
        assert!(flash_resource.reasoning_disable_supported);
        // Non-2.5 gemini has no effort thinking support -> provider sends no
        // thinkingConfig (unchanged behavior).
        let legacy = lookup("gemini", "gemini-1.5-pro");
        assert!(!legacy.thinking_modes.iter().any(|m| m == "effort"));
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

    #[test]
    fn vision_capability_gates_known_multimodal_models() {
        reset();
        let minimax_m3 = lookup("minimax", "MiniMax-M3");
        assert!(minimax_m3.vision_supported);
        assert!(minimax_m3.video);
        assert_eq!(minimax_m3.thinking_modes, vec!["adaptive"]);
        assert_eq!(minimax_m3.reasoning_wire_format.as_deref(), Some("minimax"));
        assert!(minimax_m3.requires_completion_tokens);
        let openrouter_m3 = lookup("openrouter", "minimax/minimax-m3");
        assert!(openrouter_m3.vision_supported);
        assert!(openrouter_m3.video);
        assert!(lookup("openai", "gpt-4o").vision_supported);
        assert!(lookup("openai", "gpt-5.4-preview").vision_supported);
        assert!(lookup("anthropic", "claude-sonnet-4-6").vision_supported);
        assert!(lookup("anthropic", "claude-sonnet-4-6").pdf);
        assert!(lookup("anthropic", "claude-sonnet-4-6").files_api_supported);
        assert!(lookup("openrouter", "google/gemini-2.5-flash").vision_supported);
        assert!(lookup("gemini", "gemini-2.5-flash").vision_supported);
        assert!(lookup("gemini", "gemini-2.5-flash").audio);
        assert!(lookup("gemini", "gemini-2.5-flash").pdf);
        assert_eq!(
            lookup("gemini", "gemini-2.5-flash").structured_output_mode,
            "native_json"
        );
        assert!(lookup("ollama", "llava:latest").vision_supported);
        assert!(lookup("ollama", "gemma4:26b").vision_supported);
        assert!(lookup("ollama", "gemma4-128k:latest").vision_supported);
        assert!(!lookup("openai", "gpt-3.5-turbo").vision_supported);
        assert!(!lookup("ollama", "qwen3.5:35b-a3b-coding-nvfp4").vision_supported);
    }

    #[test]
    fn computer_use_style_projects_per_provider() {
        reset();
        // Anthropic vision models get the native-Anthropic projection style,
        // XGA screenshot scaling, and advertise the `computer_use` hosted tool.
        for model in [
            "claude-opus-4-8",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-fable-5",
            "claude-haiku-4-7",
        ] {
            let caps = lookup("anthropic", model);
            assert_eq!(
                caps.computer_use_style,
                Some(super::super::ComputerUseStyle::NativeAnthropic),
                "{model} should project native_anthropic"
            );
            assert_eq!(
                caps.screenshot_scaling,
                Some(super::super::ScreenshotScaling::Xga),
                "{model}"
            );
            assert!(
                !caps.safety_ack_flow,
                "{model} does not use the OpenAI safety-ack flow"
            );
        }
        // OpenAI keeps `computer_use` in its Responses hosted-tool list; the
        // Anthropic surface is gated by `computer_use_style` instead, since
        // `hosted_tools` is a Responses-only concept `lookup` strips for
        // non-OpenAI providers.
        assert!(lookup("openai", "gpt-5.4")
            .hosted_tools
            .iter()
            .any(|t| t == "computer_use"));
        // OpenAI Responses computer models get the native-OpenAI projection,
        // identity screenshot scaling, and the safety-ack flow.
        for model in ["gpt-5.4", "gpt-5.4-preview"] {
            let caps = lookup("openai", model);
            assert_eq!(
                caps.computer_use_style,
                Some(super::super::ComputerUseStyle::NativeOpenai),
                "{model} should project native_openai"
            );
            assert_eq!(
                caps.screenshot_scaling,
                Some(super::super::ScreenshotScaling::Original),
                "{model}"
            );
            assert!(caps.safety_ack_flow, "{model} uses the safety-ack flow");
        }
        assert_eq!(
            lookup("openai", "openai/gpt-5.4").computer_use_style,
            Some(super::super::ComputerUseStyle::NativeOpenai)
        );
        // Non-computer routes leave the field unset.
        assert!(lookup("openai", "gpt-3.5-turbo")
            .computer_use_style
            .is_none());
        assert!(lookup("openai", "gpt-4o").computer_use_style.is_none());
    }

    #[test]
    fn openrouter_gemini_explicit_cache_uses_block_breakpoints() {
        reset();
        let caps = lookup("openrouter", "google/gemini-2.5-flash");
        assert!(caps.prompt_caching);
        assert_eq!(caps.cache_breakpoint_style, "last_block");
    }

    #[test]
    fn local_gemma4_exposes_native_tools_and_structured_output() {
        // Fix A: vLLM/SGLang serve Gemma 4 over the OpenAI-compatible surface,
        // so the local route must declare native tools + native structured
        // output like its hosted gemma-4 siblings — not silently fall back to
        // text tools.
        reset();
        let caps = lookup("local", "gemma-4-26b-a4b-it");
        assert!(caps.native_tools);
        assert_eq!(caps.preferred_tool_format.as_deref(), Some("native"));
        assert_eq!(caps.structured_output.as_deref(), Some("native"));
    }

    #[test]
    fn local_gemma4_exposes_vision_like_hosted_siblings() {
        // harn#3585: Gemma 4 is multimodal on every served surface. The local
        // OpenAI-compat route must declare vision so the derived structured
        // caps and emitted `capability_tags` agree with the gemini/openrouter/
        // together siblings.
        reset();
        for model in ["gemma-4-e4b-it", "gemma-4-e2b-it", "gemma-4-26b-a4b-it"] {
            let caps = lookup("local", model);
            assert!(
                caps.vision_supported,
                "local {model} should expose vision_supported"
            );
            let tags = crate::llm_config::capability_tags_from_capabilities(&caps);
            assert!(
                tags.iter().any(|t| t == "vision"),
                "local {model} emitted capability_tags should include `vision`, got {tags:?}"
            );
        }
    }

    #[test]
    fn ollama_vision_models_have_no_reasoning_scaffold() {
        // Fix B: bakllava / llama3.2-vision / gemma3 are caption/vision models
        // with no reasoning capability; they must resolve to the "none" thinking
        // block style (like the llava sibling) so the template does not emit a
        // spurious "## Reasoning" scaffold.
        reset();
        for model in ["bakllava:latest", "llama3.2-vision:11b", "gemma3:27b"] {
            assert_eq!(
                lookup("ollama", model).thinking_block_style,
                "none",
                "{model} should resolve to thinking_block_style=\"none\""
            );
        }
        // Sibling sanity check.
        assert_eq!(
            lookup("ollama", "llava:latest").thinking_block_style,
            "none"
        );
    }

    #[test]
    fn ollama_gemma4_supports_structured_output_and_text_tools() {
        // Fix C: Ollama honors the `format` kwarg, so both gemma4 rules must
        // declare structured_output="format_kw" (otherwise JSON/schema output
        // was blocked) plus explicit text tools for parity with the qwen rules.
        reset();
        for model in ["gemma4:12b-mlx", "gemma4:26b"] {
            let caps = lookup("ollama", model);
            assert_eq!(
                caps.structured_output.as_deref(),
                Some("format_kw"),
                "{model} should resolve structured_output=\"format_kw\""
            );
            assert!(!caps.native_tools, "{model} should use text tools");
            assert_eq!(
                caps.preferred_tool_format.as_deref(),
                Some("text"),
                "{model} should prefer text tool format"
            );
            assert_eq!(
                caps.thinking_block_style, "none",
                "{model} ships thinking-off"
            );
        }
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
    fn xai_grok_marks_stop_and_penalties_unsupported() {
        reset();
        // xAI returns HTTP 400 on `stop`, `frequency_penalty`, and
        // `presence_penalty` for every Grok model (live probe 2026-07-14). The
        // `grok-*` rule gates all three so no request carries an invalid field.
        let caps = lookup("xai", "grok-4.5");
        assert!(!caps.stop_supported);
        assert!(!caps.frequency_penalty_supported);
        assert!(!caps.presence_penalty_supported);
        assert!(caps.native_tools);
        assert!(caps.prompt_caching);
        assert!(caps.vision_supported);

        // The gate is the `grok-*` wildcard, so older Grok models inherit it.
        let older = lookup("xai", "grok-4.3");
        assert!(!older.stop_supported);
        assert!(!older.frequency_penalty_supported);

        // A route with no stop override keeps the default-true behavior.
        let unrestricted = lookup("openrouter", "moonshotai/kimi-k2.6");
        assert!(unrestricted.stop_supported);
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

        let glm = lookup("together", "zai-org/GLM-5.1");
        assert!(glm.native_tools);
        assert!(glm.prompt_caching);
        assert_eq!(glm.preferred_tool_format.as_deref(), Some("text"));
        assert_eq!(glm.tool_mode_parity.as_deref(), Some("native_unreliable"));
        assert_eq!(
            glm.auto_reasoning_overrides
                .get("agent")
                .map(String::as_str),
            Some("off"),
        );

        let openrouter_glm = lookup("openrouter", "z-ai/glm-5.2");
        assert!(openrouter_glm.reasoning_effort_supported);
        assert_eq!(
            openrouter_glm.reasoning_effort_levels,
            vec!["high", "xhigh", "max"]
        );
        assert_eq!(
            openrouter_glm.preferred_tool_format.as_deref(),
            Some("text")
        );

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
                caps.cache_breakpoint_style, "top_level",
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
    fn openrouter_deepseek_v32_defaults_to_text_tools() {
        reset();
        let caps = lookup("openrouter", "deepseek/deepseek-v3.2");
        assert!(caps.native_tools);
        assert!(caps.text_tool_wire_format_supported);
        assert_eq!(caps.preferred_tool_format.as_deref(), Some("text"));
        assert_eq!(caps.tool_mode_parity.as_deref(), Some("native_unreliable"));
        assert_eq!(caps.structured_output.as_deref(), Some("native"));
        assert!(caps.prompt_caching);
        assert_eq!(caps.cache_breakpoint_style, "last_block");

        let automated = lookup("openrouter", "deepseek/deepseek-v3");
        assert!(automated.prompt_caching);
        assert_eq!(automated.cache_breakpoint_style, "none");
    }

    #[test]
    fn openrouter_explicit_cache_routes_get_block_breakpoints() {
        reset();
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
                caps.cache_breakpoint_style, "last_block",
                "{model} should request explicit content-block cache breakpoints",
            );
        }

        let open_weight = lookup("openrouter", "qwen/qwen3.6-35b-a3b");
        assert!(!open_weight.prompt_caching);
        assert_eq!(open_weight.cache_breakpoint_style, "none");
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
    fn bedrock_claude_uses_anthropic_wire_capabilities() {
        reset();
        let caps = lookup("bedrock", "anthropic.claude-3-5-sonnet-20240620-v1:0");
        assert!(caps.native_tools);
        assert_eq!(caps.message_wire_format, WireDialect::Anthropic);
        assert_eq!(caps.native_tool_wire_format, "anthropic");
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

    #[test]
    fn cerebras_gpt_oss_declares_supported_reasoning_efforts() {
        // Cerebras GPT-OSS accepts low/medium/high only. The policy resolver
        // uses this list to floor `reasoning_policy: "off"` to `low` instead
        // of sending unsupported `none` or `minimal` values.
        reset();
        let caps = lookup("cerebras", "gpt-oss-120b");
        assert_cerebras_effort_reasoning("gpt-oss-120b", "reasoning_summary");
        assert!(!caps.reasoning_none_supported);
        assert_eq!(caps.reasoning_effort_levels, vec!["low", "medium", "high"]);
    }

    #[test]
    fn gpt_oss_requires_reasoning_for_tools_with_provider_specific_tool_wire() {
        // gpt-oss (Harmony) calls tools INSIDE the chain-of-thought channel, so
        // reasoning-off breaks tool calling. Provider catch-all rules carry no
        // reasoning fields, so without a dedicated `*gpt-oss*` row gpt-oss
        // would fall through to reasoning-OFF and the eval loop would bill a
        // noncommittal. Tool wire support is provider-specific: the pay-per-token
        // routes (OpenRouter, Fireworks, DeepInfra, SambaNova) ride Harn's TEXT
        // channel — their provider-native Harmony path drops tool calls into the
        // reasoning/commentary channel (empty `tool_calls` / billed-noncommittal,
        // see the DeepInfra/SambaNova rows + vLLM #22578/#44216, SGLang
        // #8976/#10738, openai/harmony #68). Within the text channel they use the
        // escape-free heredoc (`text`) grammar rather than fenced-JSON, because
        // gpt-oss double-escapes the backslashes a JSON string arg requires and
        // corrupts `\\`-heavy code bodies (empirical A/B 2026-06-21: text beats
        // json on both dispatch and byte-fidelity). Only the native-clean direct
        // routes (Cerebras, Groq) still use provider-native tools.
        reset();
        for (provider, model, native_tools, preferred_tool_format) in [
            ("openrouter", "openai/gpt-oss-120b", false, "text"),
            (
                "fireworks",
                "accounts/fireworks/models/gpt-oss-120b",
                false,
                "text",
            ),
            ("deepinfra", "openai/gpt-oss-120b", false, "text"),
            ("sambanova", "sambanova/gpt-oss-120b", false, "text"),
            ("cerebras", "gpt-oss-120b", true, "native"),
            ("groq", "openai/gpt-oss-120b", true, "native"),
        ] {
            let caps = lookup(provider, model);
            assert!(
                caps.reasoning_required_for_tools,
                "{provider}/{model}: reasoning_required_for_tools must be true"
            );
            assert!(
                caps.reasoning_effort_supported,
                "{provider}/{model}: reasoning_effort_supported must be true"
            );
            assert_eq!(
                caps.reasoning_effort_levels,
                vec!["low", "medium", "high"],
                "{provider}/{model}: effort levels"
            );
            assert_eq!(caps.thinking_modes, vec!["effort"], "{provider}/{model}");
            assert_eq!(
                caps.native_tools, native_tools,
                "{provider}/{model}: native_tools"
            );
            assert_eq!(
                caps.preferred_tool_format.as_deref(),
                Some(preferred_tool_format),
                "{provider}/{model}: preferred tool format"
            );
            assert_eq!(
                caps.thinking_block_style, "reasoning_summary",
                "{provider}/{model}"
            );
        }
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
    fn mock_with_claude_model_routes_to_anthropic() {
        reset();
        let caps = lookup("mock", "claude-sonnet-4-7");
        assert!(caps.defer_loading);
        assert_eq!(caps.tool_search, vec!["bm25", "regex"]);
    }

    #[test]
    fn mock_with_gpt_model_routes_to_openai() {
        reset();
        let caps = lookup("mock", "gpt-5.4-preview");
        assert!(caps.defer_loading);
        assert_eq!(caps.tool_search, vec!["hosted", "client"]);
    }

    #[test]
    fn mock_with_gemini_model_routes_to_gemini() {
        reset();
        let caps = lookup("mock", "gemini-2.5-flash");
        assert_eq!(caps.message_wire_format, WireDialect::Gemini);
        assert_eq!(caps.native_tool_wire_format, "openai");
        assert!(caps.prefers_xml_scaffolding);
    }

    #[test]
    fn qwen36_ollama_preserves_thinking() {
        reset();
        let caps = lookup("ollama", "qwen3.6:35b-a3b-coding-nvfp4");
        assert!(!caps.native_tools);
        assert_eq!(caps.json_schema.as_deref(), Some("format_kw"));
        assert!(!caps.thinking_modes.is_empty());
        assert!(
            caps.preserve_thinking,
            "Qwen3.6 should enable preserve_thinking by default for long-horizon loops"
        );
        assert_eq!(caps.server_parser, "none");
        assert!(!caps.honors_chat_template_kwargs);
        assert_eq!(caps.recommended_endpoint.as_deref(), Some("/api/chat"));
        assert!(caps.text_tool_wire_format_supported);
        assert!(caps.prefers_markdown_scaffolding);
        assert_eq!(caps.structured_output_mode, "delimited");
        assert!(!caps.prefers_xml_tools);
        assert_eq!(caps.thinking_block_style, "inline");
        // Inline thinking_block_style routes emit their reasoning as inline
        // `<think>` blocks in the text channel, so the derived quirk is on.
        assert!(caps.emits_inline_reasoning);
    }

    #[test]
    fn emits_inline_reasoning_tracks_inline_thinking_block_style() {
        reset();
        // Inline-style local/open-weight routes emit inline `<think>` in text.
        assert!(lookup("ollama", "qwen3.6:35b-a3b-coding-nvfp4").emits_inline_reasoning);
        assert!(lookup("moonshot", "moonshot/kimi-k2.6").emits_inline_reasoning);
        assert!(lookup("cerebras", "zai-glm-4.7").emits_inline_reasoning);
        // Hosted providers surface reasoning in a dedicated channel, not inline
        // `<think>` in the text body — the quirk stays off so their text passes
        // through the envelope untouched.
        assert!(!lookup("anthropic", "claude-sonnet-5").emits_inline_reasoning);
        assert!(!lookup("openai", "gpt-5.4").emits_inline_reasoning);
        assert!(!lookup("ollama", "llava:latest").emits_inline_reasoning);
    }

    #[test]
    fn qwen35_ollama_does_not_preserve_thinking() {
        reset();
        let caps = lookup("ollama", "qwen3.5:35b-a3b-coding-nvfp4");
        assert!(caps.native_tools);
        assert!(!caps.thinking_modes.is_empty());
        assert!(
            !caps.preserve_thinking,
            "Qwen3.5 lacks the preserve_thinking kwarg — rely on the chat template's rolling checkpoint instead"
        );
        assert_eq!(caps.server_parser, "ollama_qwen3coder");
        assert!(!caps.text_tool_wire_format_supported);
    }

    #[test]
    fn qwen36_routed_providers_all_preserve_thinking() {
        reset();
        for (provider, model) in [
            ("openrouter", "qwen/qwen3.6-plus"),
            ("together", "Qwen/Qwen3.6-Plus"),
            ("huggingface", "Qwen/Qwen3.6-35B-A3B"),
            ("fireworks", "accounts/fireworks/models/qwen3p6-plus"),
            ("dashscope", "qwen3.6-plus"),
            ("local", "Qwen3.6-35B-A3B"),
            ("mlx", "unsloth/Qwen3.6-35B-A3B-UD-MLX-4bit"),
            ("mlx", "Qwen/Qwen3.6-35B-A3B"),
        ] {
            let caps = lookup(provider, model);
            assert!(
                !caps.thinking_modes.is_empty(),
                "{provider}/{model}: thinking"
            );
            assert!(
                caps.preserve_thinking,
                "{provider}/{model}: preserve_thinking must be on for Qwen3.6"
            );
            assert!(caps.native_tools, "{provider}/{model}: native_tools");
            assert_ne!(
                caps.server_parser, "ollama_qwen3coder",
                "{provider}/{model}: only Ollama routes through the qwen3coder response parser"
            );
        }

        let caps = lookup("llamacpp", "unsloth/Qwen3.6-35B-A3B-GGUF");
        assert!(!caps.thinking_modes.is_empty());
        assert!(caps.preserve_thinking);
        assert!(!caps.native_tools);
        assert!(caps.text_tool_wire_format_supported);
        assert_eq!(caps.server_parser, "none");
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

    #[test]
    fn devstral_local_routes_default_to_json_tools() {
        reset();
        for provider in ["ollama", "llamacpp"] {
            let caps = lookup(provider, "devstral-small-2:24b");
            assert!(!caps.native_tools, "{provider}: native tools stay opt-in");
            assert!(
                caps.text_tool_wire_format_supported,
                "{provider}: text tools should remain available"
            );
            // devstral has no reserved-token constraint, so it uses the global
            // `json` (fenced-JSON) text-channel default. Heredoc stays
            // reachable via an explicit `preferred_tool_format = "text"` pin.
            assert_eq!(
                caps.preferred_tool_format.as_deref(),
                Some("json"),
                "{provider}: devstral inherits the global json default"
            );
        }
    }

    #[test]
    fn openrouter_mistral_routes_use_native_tools() {
        reset();
        let caps = lookup("openrouter", "mistralai/mistral-small-2603");
        assert!(caps.native_tools);
        assert!(caps.text_tool_wire_format_supported);
        assert_eq!(caps.structured_output.as_deref(), Some("native"));
        assert_eq!(caps.structured_output_mode, "native_json");
    }

    #[test]
    fn dashscope_and_llamacpp_resolve_capabilities() {
        reset();
        // New sibling providers should fall through to `openai` for
        // gpt-*  models even without dedicated rules.
        let caps = lookup("dashscope", "gpt-5.4-preview");
        assert!(caps.defer_loading);
        let caps = lookup("llamacpp", "gpt-5.4-preview");
        assert!(caps.defer_loading);
    }

    #[test]
    fn unknown_provider_has_no_capabilities() {
        reset();
        let caps = lookup("my-custom-proxy", "foo-bar-1");
        assert!(!caps.native_tools);
        assert!(!caps.defer_loading);
        assert!(caps.tool_search.is_empty());
    }

    #[test]
    fn openrouter_specific_rules_win_and_family_inheritance_is_preserved() {
        // Capability resolution is first-match-wins over fragment order
        // (`first_matching_rule_in_file` -> `Iterator::find`), and when no
        // `provider.openrouter` rule matches it walks the `[provider_family]`
        // chain (openrouter -> openai). Both contracts must hold so that:
        //   1. a specific OpenRouter carve-out beats a broader OpenRouter rule,
        //   2. gpt-/o-family slugs routed through OpenRouter still inherit the
        //      rich openai-family capability set (a blanket `*` openrouter row
        //      would shadow this — see the catalog-or-defaults report).
        reset();

        // 1. Specific carve-out wins: deepseek/deepseek-v3.2 is pinned to the
        // Harn text-tool channel even though the broader deepseek/deepseek-v3*
        // rule below it would otherwise resolve `native`.
        let deepseek = lookup("openrouter", "deepseek/deepseek-v3.2");
        assert_eq!(
            deepseek.preferred_tool_format.as_deref(),
            Some("text"),
            "deepseek-v3.2 text carve-out must win over the broader deepseek-v3* rule"
        );
        assert_eq!(
            deepseek.tool_mode_parity.as_deref(),
            Some("native_unreliable")
        );
        // The broader sibling still resolves native for non-3.2 v3 slugs.
        assert_eq!(
            lookup("openrouter", "deepseek/deepseek-v3-base")
                .preferred_tool_format
                .as_deref(),
            Some("native")
        );

        // 2. Family inheritance preserved: an openai-prefixed slug routed via
        // OpenRouter still picks up openai-family reasoning fields.
        let prefixed = lookup("openrouter", "openai/o4-mini");
        assert!(prefixed.requires_completion_tokens);
        assert!(prefixed.reasoning_effort_supported);

        // The newly added MiniMax M2.5 OR mirror resolves native via the
        // existing `minimax/minimax-m2*` rule.
        let m25 = lookup("openrouter", "minimax/minimax-m2.5");
        assert!(m25.native_tools);
        assert_eq!(m25.preferred_tool_format.as_deref(), Some("native"));
    }

    #[test]
    fn enterprise_routes_expose_format_preferences() {
        reset();
        let bedrock_claude = lookup("bedrock", "anthropic.claude-opus-4-7-v1:0");
        assert!(bedrock_claude.prefers_xml_scaffolding);
        assert_eq!(bedrock_claude.structured_output_mode, "xml_tagged");
        assert!(!bedrock_claude.supports_assistant_prefill);
        assert!(bedrock_claude.prefers_xml_tools);

        let azure_o = lookup("azure_openai", "o3-prod");
        assert!(azure_o.prefers_markdown_scaffolding);
        assert_eq!(azure_o.structured_output_mode, "native_json");
        assert!(azure_o.prefers_role_developer);
        assert_eq!(azure_o.thinking_block_style, "reasoning_summary");
    }

    #[test]
    fn user_override_adds_new_provider() {
        reset();
        let toml_src = concat!(
            "[[provider.my-proxy]]\n",
            "model_match = \"*\"\n",
            "native_tools = true\n",
            "tool_search = [\"hosted\"]\n",
            "prefers_xml_scaffolding = true\n",
            "structured_output_mode = \"xml_tagged\"\n",
            "supports_assistant_prefill = true\n",
            "prefers_xml_tools = true\n",
            "thinking_block_style = \"thinking_blocks\"\n",
        );
        set_user_overrides_toml(toml_src).unwrap();
        let caps = lookup("my-proxy", "anything");
        assert!(caps.native_tools);
        assert_eq!(caps.tool_search, vec!["hosted"]);
        assert!(caps.prefers_xml_scaffolding);
        assert_eq!(caps.structured_output_mode, "xml_tagged");
        assert!(caps.supports_assistant_prefill);
        assert!(caps.prefers_xml_tools);
        assert_eq!(caps.thinking_block_style, "thinking_blocks");
        clear_user_overrides();
    }

    #[test]
    fn user_override_takes_precedence_over_builtin() {
        reset();
        let toml_src = r#"
[[provider.anthropic]]
model_match = "claude-opus-*"
native_tools = true
defer_loading = false
tool_search = []
"#;
        set_user_overrides_toml(toml_src).unwrap();
        let caps = lookup("anthropic", "claude-opus-4-7");
        assert!(caps.native_tools);
        assert!(!caps.defer_loading);
        assert!(caps.tool_search.is_empty());
        clear_user_overrides();
    }

    #[test]
    fn user_override_from_manifest_toml() {
        reset();
        let manifest = concat!(
            "[package]\n",
            "name = \"demo\"\n\n",
            "[[capabilities.provider.my-proxy]]\n",
            "model_match = \"*\"\n",
            "native_tools = true\n",
            "tool_search = [\"hosted\"]\n",
            "prefers_markdown_scaffolding = true\n",
            "structured_output_mode = \"native_json\"\n",
            "prefers_role_developer = true\n",
            "thinking_block_style = \"reasoning_summary\"\n",
        );
        set_user_overrides_from_manifest_toml(manifest).unwrap();
        let caps = lookup("my-proxy", "foo");
        assert!(caps.native_tools);
        assert_eq!(caps.tool_search, vec!["hosted"]);
        assert!(caps.prefers_markdown_scaffolding);
        assert_eq!(caps.structured_output_mode, "native_json");
        assert!(caps.prefers_role_developer);
        assert_eq!(caps.thinking_block_style, "reasoning_summary");
        clear_user_overrides();
    }

    #[test]
    fn version_min_requires_parseable_model() {
        reset();
        let toml_src = r#"
[[provider.custom]]
model_match = "*"
version_min = [5, 4]
native_tools = true
"#;
        set_user_overrides_toml(toml_src).unwrap();
        // Unparseable model ID + version_min → rule doesn't match.
        let caps = lookup("custom", "mystery-model");
        assert!(!caps.native_tools);
        clear_user_overrides();
    }

    #[test]
    fn openrouter_namespaced_anthropic_model() {
        reset();
        let caps = lookup("anthropic", "anthropic/claude-opus-4-7");
        assert!(caps.defer_loading);
    }
}
