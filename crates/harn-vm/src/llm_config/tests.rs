use super::*;
use harn_glob::match_name as glob_match;
use std::collections::BTreeMap;

fn reset_overrides() {
    clear_user_overrides();
    clear_runtime_provider_endpoint_overrides();
}

#[test]
fn resolve_model_info_guards_bad_native_pin_on_unreliable_route() {
    reset_overrides();
    // An alias that pins tool_format = "native" for DeepSeek V3.2 on
    // OpenRouter — a route the capability registry knows is
    // native_unreliable (drops to unparsed DSML text). Before the
    // footgun-removal gate this bad pin survived resolution verbatim and
    // produced vanishing tool calls; now it is steered to the route's safe
    // text-channel format.
    let overlay = parse_config_toml(
            "[aliases.guard-ds]\nid = \"deepseek/deepseek-v3.2\"\nprovider = \"openrouter\"\ntool_format = \"native\"\n",
        )
        .expect("overlay parses");
    set_user_overrides(Some(overlay));
    let resolved = resolve_model_info("guard-ds");
    assert_eq!(
        resolved.tool_format, "text",
        "a native pin on a native_unreliable route must be auto-corrected to text"
    );
    clear_user_overrides();

    // A safe native pin (a route with no adverse parity) is untouched.
    let overlay_ok = parse_config_toml(
            "[aliases.guard-ds-ok]\nid = \"deepseek/deepseek-v3-base\"\nprovider = \"openrouter\"\ntool_format = \"native\"\n",
        )
        .expect("overlay parses");
    set_user_overrides(Some(overlay_ok));
    let resolved_ok = resolve_model_info("guard-ds-ok");
    assert_eq!(resolved_ok.tool_format, "native");
    clear_user_overrides();
}

#[test]
fn auto_select_prefers_local_provider_without_cloud_credentials() {
    // A catalog whose only provider is local and auth-free resolves to it
    // regardless of ambient cloud API keys: no preferred/credentialed cloud
    // provider is present, so the local fallback wins deterministically.
    let config = parse_config_toml(
            "[providers.ollama]\nbase_url = \"http://localhost:11434\"\nchat_endpoint = \"/v1/chat/completions\"\n",
        )
        .expect("config parses");
    assert!(provider_is_local(config.providers.get("ollama").unwrap()));
    assert_eq!(auto_select_provider(&config), "ollama");
}

#[test]
fn auto_select_falls_back_to_documented_default_when_empty() {
    let config = parse_config_toml("").expect("config parses");
    assert_eq!(auto_select_provider(&config), FALLBACK_PROVIDER);
}

#[test]
fn suppress_routes_parse_and_merge_dedupe() {
    let mut base =
        parse_config_toml("[suppress]\nroutes = [\"together:Qwen/Qwen3-Coder-Next-FP8\"]\n")
            .expect("base parses");
    assert!(!base.is_empty(), "a suppress-only overlay is not empty");
    let overlay = parse_config_toml(
        "[suppress]\nroutes = [\"together:Qwen/Qwen3-Coder-Next-FP8\", \"ollama:img:tag\"]\n",
    )
    .expect("overlay parses");
    base.merge_from(&overlay);
    assert_eq!(
        base.suppress.routes,
        vec![
            "together:Qwen/Qwen3-Coder-Next-FP8".to_string(),
            "ollama:img:tag".to_string(),
        ],
        "merge appends new selectors without duplicating existing ones"
    );
}

#[test]
fn test_glob_match_prefix() {
    assert!(glob_match("claude-*", "claude-sonnet-4-20250514"));
    assert!(glob_match("gpt-*", "gpt-4o"));
    assert!(!glob_match("claude-*", "gpt-4o"));
}

#[test]
fn test_glob_match_suffix() {
    assert!(glob_match("*-latest", "llama3.2-latest"));
    assert!(!glob_match("*-latest", "llama3.2"));
}

#[test]
fn test_glob_match_middle() {
    assert!(glob_match("claude-*-latest", "claude-sonnet-latest"));
    assert!(!glob_match("claude-*-latest", "claude-sonnet-beta"));
}

#[test]
fn test_glob_match_exact() {
    assert!(glob_match("gpt-4o", "gpt-4o"));
    assert!(!glob_match("gpt-4o", "gpt-4o-mini"));
}

#[test]
fn test_infer_provider_from_defaults() {
    let _guard = crate::llm::env_guard();
    let _env = crate::test_env::test_env_guard();

    assert_eq!(infer_provider("claude-sonnet-4-20250514"), "anthropic");
    assert_eq!(infer_provider("gpt-4o"), "openai");
    assert_eq!(infer_provider("o1-preview"), "openai");
    assert_eq!(infer_provider("o3-mini"), "openai");
    assert_eq!(infer_provider("o4-mini"), "openai");
    assert_eq!(infer_provider("gemini-2.5-pro"), "gemini");
    assert_eq!(infer_provider("qwen/qwen3-coder"), "openrouter");
    assert_eq!(infer_provider("llama3.2:latest"), "ollama");
    assert_eq!(infer_provider("unknown-model"), "anthropic");
}

#[test]
fn test_openrouter_inference_requires_one_slash() {
    let _guard = crate::llm::env_guard();
    let _env = crate::test_env::test_env_guard();

    assert_eq!(infer_provider("org/model"), "openrouter");
    assert_eq!(infer_provider("org/team/model"), "anthropic");
}

#[test]
fn test_cerebras_inference_beats_openrouter_slash_fallback() {
    let _guard = crate::llm::env_guard();
    let _env = crate::test_env::test_env_guard();

    assert_eq!(infer_provider("cerebras/gpt-oss-120b"), "cerebras");
    assert_eq!(infer_provider("cerebras/zai-glm-4.7"), "cerebras");
    assert_eq!(infer_provider("cerebras/llama-3.3-70b"), "cerebras");
}

#[test]
fn test_direct_catalog_model_id_resolves_to_catalog_provider() {
    // Bare model IDs that the embedded catalog hosts on Cerebras must
    // not be misrouted by the generic `gpt-*` / single-slash inference
    // fallbacks. Regression for harn#2142 (model-info routed
    // `gpt-oss-120b` to openai, breaking host TUI credential checks).
    let _guard = crate::llm::env_guard();
    let _env = crate::test_env::test_env_guard();

    for model in ["gpt-oss-120b", "zai-glm-4.7", "llama-3.3-70b"] {
        assert_eq!(
            infer_provider(model),
            "cerebras",
            "{model} should route to its catalog provider"
        );
        let resolved = resolve_model_info(model);
        assert_eq!(resolved.id, model);
        assert_eq!(resolved.provider, "cerebras");
    }
}

#[test]
fn test_equivalent_model_catalog_entries_use_capability_compatible_routes() {
    reset_overrides();

    assert_eq!(
        wire_model_id("groq/openai/gpt-oss-120b"),
        "openai/gpt-oss-120b"
    );
    assert_eq!(wire_model_id("gpt-oss-120b"), "gpt-oss-120b");

    let equivalents = equivalent_model_catalog_entries("gpt-oss-120b");
    let ids = equivalents
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();

    assert!(
        ids.contains(&"groq/openai/gpt-oss-120b"),
        "Cerebras GPT-OSS should surface the Groq serving variant"
    );
    assert!(
        !ids.contains(&"gpt-oss-120b"),
        "equivalence results should not include the source row"
    );
    assert!(equivalents
        .iter()
        .all(|(_, model)| { model.equivalence_group.as_deref() == Some("openai-gpt-oss-120b") }));
}

#[test]
fn frontier_agent_equivalents_use_request_sized_context_for_failover() {
    reset_overrides();

    let conservative = equivalent_model_catalog_entries("claude-sonnet-4-6");
    assert!(
        !conservative
            .iter()
            .any(|(id, _)| id == "deepseek-ai/DeepSeek-V4-Pro"),
        "full-window equivalence should not claim a 512K route covers every 1M Sonnet request"
    );

    let request_sized =
        equivalent_model_catalog_entries_for_context("claude-sonnet-4-6", Some(128_000));
    let ids = request_sized
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>();

    assert!(
        ids.contains(&"deepseek-ai/DeepSeek-V4-Pro"),
        "Together DeepSeek should be a request-sized Sonnet failover candidate"
    );
    assert!(
        ids.contains(&"glm-5.2") || ids.contains(&"z-ai/glm-5.2"),
        "GLM 5.2 should stay in the frontier coding failover group"
    );
    assert!(request_sized
        .iter()
        .all(|(_, model)| { model.equivalence_group.as_deref() == Some("frontier-agent-coding") }));
}

#[test]
fn fireworks_gpt_oss_route_has_real_context_window() {
    // Regression: the Fireworks-served `accounts/fireworks/models/gpt-oss-120b`
    // wire id had NO catalog row, so its context window resolved to None and
    // the agent's auto-compaction budget had nothing to enforce — the prompt
    // grew until Fireworks rejected the turn with HTTP 400 [context_overflow]
    // (session 019ee303: 197467 tokens > 131071 max). Cataloging the real
    // 131072 window lets compaction trigger before the hard limit.
    reset_overrides();

    let entry = model_catalog_entry("accounts/fireworks/models/gpt-oss-120b")
        .expect("Fireworks gpt-oss-120b must be in the model catalog");
    assert_eq!(entry.context_window, 131_072);
    assert_eq!(entry.provider, "fireworks");
    assert_eq!(
        entry.equivalence_group.as_deref(),
        Some("openai-gpt-oss-120b"),
    );
}

#[test]
fn test_user_catalog_overlay_re_homes_model_provider() {
    // Exact-match catalog lookup must honor user overlays as well as embedded TOML.
    reset_overrides();
    let mut overlay = ProvidersConfig::default();
    overlay.models.insert(
        "gpt-4o".to_string(),
        ModelDef {
            name: "GPT-4o via OpenRouter".to_string(),
            display_name: None,
            blurb: None,
            provider: "openrouter".to_string(),
            context_window: 128_000,
            logical_model: None,
            equivalence_group: None,
            served_variant: None,
            wire_model: None,
            api_dialect: None,
            rate_limits: None,
            performance: None,
            architecture: None,
            local_memory: None,
            runtime_context_window: None,
            stream_timeout: None,
            capabilities: Vec::new(),
            pricing: None,
            deprecated: false,
            deprecation_note: None,
            superseded_by: None,
            serving_tiers: Vec::new(),
            quality_tags: Vec::new(),
            availability: ModelAvailability::default(),
            tier: None,
            open_weight: None,
            strengths: Vec::new(),
            benchmarks: std::collections::BTreeMap::new(),
            family: None,
            lineage: None,
            complementary_with: Vec::new(),
            avoid_as_reviewer_for: Vec::new(),
        },
    );
    set_user_overrides(Some(overlay));

    assert_eq!(infer_provider("gpt-4o"), "openrouter");

    reset_overrides();
}

#[test]
fn test_model_tier_from_defaults() {
    // Tier is now self-declared per model row in providers.toml.
    // Models that match an entry use the declared value; unknown
    // model ids fall through to `tier_defaults.default` ("mid").
    assert_eq!(model_tier("claude-sonnet-4-20250514"), "frontier");
    assert_eq!(model_tier("gpt-4o"), "frontier");
    assert_eq!(model_tier("Qwen/Qwen3.5-9B"), "small");
    assert_eq!(model_tier("deepseek-v4-flash"), "mid");
    assert_eq!(model_tier("deepseek-v4-pro"), "frontier");
    assert_eq!(model_tier("MiniMax-M2.7"), "frontier");
    assert_eq!(model_tier("glm-5.1"), "frontier");
    // Unknown ids resolve to the default.
    assert_eq!(model_tier("definitely-not-a-real-model"), "mid");
}

#[test]
fn test_model_family_preserves_underlying_hosted_lineage() {
    assert_eq!(
        model_family("openrouter", "anthropic/claude-sonnet-4-6"),
        "anthropic-claude"
    );
    assert_eq!(
        model_family("openrouter", "google/gemini-2.5-flash"),
        "google-gemini"
    );
    assert_eq!(
        model_family("openrouter", "openai/o3-mini"),
        "openai-reasoning"
    );
    assert_eq!(model_lineage("openrouter", "openai/gpt-5.5"), "openai-gpt5");
    assert_eq!(
        model_lineage("openrouter", "openai/o3-mini"),
        "openai-reasoning"
    );
    assert_eq!(
        model_lineage("anthropic", "claude-opus-4-8"),
        "claude-opus-adaptive"
    );
    assert_eq!(model_lineage("llamacpp", "qwen3.6-35b-a3b"), "qwen3");
}

#[test]
fn test_resolve_model_unknown_alias() {
    let (id, provider) = resolve_model("gpt-4o");
    assert_eq!(id, "gpt-4o");
    assert!(provider.is_none());
}

#[test]
fn test_provider_names() {
    let names = provider_names();
    assert!(names.len() >= 7);
    assert!(names.contains(&"anthropic".to_string()));
    assert!(names.contains(&"together".to_string()));
    assert!(names.contains(&"local".to_string()));
    assert!(names.contains(&"mlx".to_string()));
    assert!(names.contains(&"openai".to_string()));
    assert!(names.contains(&"ollama".to_string()));
    assert!(names.contains(&"bedrock".to_string()));
    assert!(names.contains(&"azure_openai".to_string()));
    assert!(names.contains(&"vertex".to_string()));
}

#[test]
fn global_provider_file_is_an_overlay_on_builtin_defaults() {
    let mut overlay = ProvidersConfig {
        default_provider: Some("ollama".to_string()),
        ..Default::default()
    };
    overlay.aliases.insert(
        "quickstart".to_string(),
        AliasDef {
            id: "llama3.2".to_string(),
            provider: "ollama".to_string(),
            tool_format: None,
        },
    );

    let merged = merge_global_config(overlay);

    assert_eq!(merged.default_provider.as_deref(), Some("ollama"));
    assert!(merged.providers.contains_key("anthropic"));
    assert!(merged.providers.contains_key("ollama"));
    assert_eq!(merged.aliases["quickstart"].id, "llama3.2");
}

#[test]
fn partial_provider_overlay_preserves_builtin_provider_metadata() {
    let overlay = parse_config_toml(
        r#"
            [providers.ollama]
            base_url = "http://localhost:11435"
            extra_headers = { "x-local" = "1" }
            "#,
    )
    .expect("provider overlay parses");

    let merged = merge_global_config(overlay);
    let ollama = merged
        .providers
        .get("ollama")
        .expect("ollama remains configured");

    assert_eq!(ollama.base_url, "http://localhost:11435");
    assert_eq!(ollama.auth_style, "none");
    assert_eq!(ollama.chat_endpoint, "/api/chat");
    assert_eq!(ollama.completion_endpoint.as_deref(), Some("/api/generate"));
    assert_eq!(ollama.cost_per_1k_in, Some(0.0));
    assert_eq!(ollama.cost_per_1k_out, Some(0.0));
    assert_eq!(
        ollama
            .healthcheck
            .as_ref()
            .and_then(|healthcheck| healthcheck.path.as_deref()),
        Some("/api/tags")
    );
    assert_eq!(
        ollama.extra_headers.get("x-local").map(String::as_str),
        Some("1")
    );
}

#[test]
fn partial_provider_overlay_can_explicitly_replace_default_auth_style() {
    let overlay = parse_config_toml(
        r#"
            [providers.ollama]
            auth_style = "bearer"
            auth_env = "OLLAMA_API_KEY"
            "#,
    )
    .expect("provider overlay parses");

    let merged = merge_global_config(overlay);
    let ollama = merged
        .providers
        .get("ollama")
        .expect("ollama remains configured");

    assert_eq!(ollama.auth_style, "bearer");
    assert_eq!(auth_env_names(&ollama.auth_env), vec!["OLLAMA_API_KEY"]);
    assert_eq!(ollama.chat_endpoint, "/api/chat");
}

#[test]
fn test_resolve_tier_model_default_aliases() {
    // Exercise the alias-resolution machinery, not the specific catalog
    // value: the model under each tier alias evolves as the embedded
    // providers.toml is updated. The invariants worth pinning are the
    // provider routing + catalog-registration of the resolved model.
    let (model, provider) = resolve_tier_model("frontier", None)
        .expect("frontier alias must resolve from the embedded catalog");
    assert_eq!(provider, "anthropic");
    assert!(
        model_catalog_entry(&model)
            .is_some_and(|entry| entry.provider == "anthropic" && !entry.deprecated),
        "frontier alias must point at a registered, non-deprecated anthropic model (got {model})"
    );

    let (model, provider) = resolve_tier_model("small", None)
        .expect("small alias must resolve from the embedded catalog");
    assert!(
        [
            "openrouter",
            "huggingface",
            "local",
            "llamacpp",
            "mlx",
            "ollama"
        ]
        .contains(&provider.as_str()),
        "small tier should resolve to an open-weight provider (got {provider} / {model})"
    );

    let (model, provider) =
        resolve_tier_model("mid", None).expect("mid alias must resolve from the embedded catalog");
    assert_eq!(provider, "openrouter");
    assert_eq!(model, "qwen/qwen3-coder-next");
}

#[test]
fn test_resolve_tier_model_prefers_provider_scoped_aliases() {
    // tier/<provider> takes precedence over generic tier when the
    // caller scopes by provider. Don't pin the specific model — the
    // catalog evolves.
    let (model, provider) =
        resolve_tier_model("mid", Some("openai")).expect("mid tier scoped to openai must resolve");
    assert_eq!(provider, "openai");
    let entry = model_catalog_entry(&model).unwrap_or_else(|| {
        panic!("mid/openai alias must point at a registered model (got {model})")
    });
    assert_eq!(entry.tier.as_deref(), Some("mid"));
}

#[test]
fn test_provider_config_anthropic() {
    let pdef = provider_config("anthropic").unwrap();
    assert_eq!(pdef.auth_style, "header");
    assert_eq!(pdef.auth_header.as_deref(), Some("x-api-key"));
}

#[test]
fn test_provider_config_mlx() {
    let pdef = provider_config("mlx").unwrap();
    assert_eq!(pdef.base_url, "http://127.0.0.1:8002");
    assert_eq!(pdef.base_url_env.as_deref(), Some("MLX_BASE_URL"));
    assert_eq!(
        pdef.healthcheck.unwrap().path.as_deref(),
        Some("/v1/models")
    );

    let (model, provider) = resolve_model("mlx-qwen36-27b");
    assert_eq!(model, "unsloth/Qwen3.6-35B-A3B-UD-MLX-4bit");
    assert_eq!(provider.as_deref(), Some("mlx"));
}

#[test]
fn test_enterprise_provider_defaults_and_inference() {
    let bedrock = provider_config("bedrock").unwrap();
    assert_eq!(bedrock.auth_style, "aws_sigv4");
    assert_eq!(bedrock.base_url_env.as_deref(), Some("BEDROCK_BASE_URL"));
    assert_eq!(
        infer_provider("anthropic.claude-3-5-sonnet-20240620-v1:0"),
        "bedrock"
    );
    assert_eq!(infer_provider("meta.llama3-70b-instruct-v1:0"), "bedrock");

    let azure = provider_config("azure_openai").unwrap();
    assert_eq!(azure.base_url_env.as_deref(), Some("AZURE_OPENAI_ENDPOINT"));
    assert_eq!(
        auth_env_names(&azure.auth_env),
        vec![
            "AZURE_OPENAI_API_KEY".to_string(),
            "AZURE_OPENAI_AD_TOKEN".to_string(),
            "AZURE_OPENAI_BEARER_TOKEN".to_string(),
        ]
    );

    let vertex = provider_config("vertex").unwrap();
    assert_eq!(vertex.base_url, "https://aiplatform.googleapis.com/v1");
    assert_eq!(infer_provider("gemini-1.5-pro-002"), "gemini");
}

#[test]
fn test_default_provider_env_override_for_unknown_model() {
    let _guard = crate::llm::env_guard();
    let env = crate::test_env::test_env_guard();
    env.set("HARN_DEFAULT_PROVIDER", "openai");

    let inference = infer_provider_detail("unknown-model");

    assert_eq!(inference.provider, "openai");
    assert_eq!(
        inference.source,
        crate::llm::provider::ProviderInferenceSource::DefaultFallback
    );
}

#[test]
fn test_unknown_model_family_ignores_default_provider_fallback() {
    let _guard = crate::llm::env_guard();
    let env = crate::test_env::test_env_guard();
    env.set("HARN_DEFAULT_PROVIDER", "ollama");

    let unknown = resolve_model_info("mystery-model-xyz");
    let known_family = resolve_model_info("deepseek-mystery-model");

    assert_eq!(unknown.provider, "ollama");
    assert_eq!(unknown.family, "unknown");
    assert_eq!(unknown.lineage, "unknown");
    assert_eq!(known_family.family, "deepseek");
    assert_eq!(known_family.lineage, "deepseek");
}

#[test]
fn test_resolve_base_url_region_env() {
    let _guard = crate::llm::env_guard();
    unsafe {
        std::env::remove_var("HARN_TEST_BASE_URL");
        std::env::set_var("HARN_TEST_REGION", "CN");
    }
    let pdef = ProviderDef {
        base_url: "https://global.example/v1".to_string(),
        base_url_env: Some("HARN_TEST_BASE_URL".to_string()),
        region_env: Some("HARN_TEST_REGION".to_string()),
        regions: BTreeMap::from([
            (
                "global".to_string(),
                ProviderRegionDef {
                    base_url: "https://global.example/v1".to_string(),
                    ..Default::default()
                },
            ),
            (
                "cn".to_string(),
                ProviderRegionDef {
                    base_url: "https://cn.example/v1".to_string(),
                    ..Default::default()
                },
            ),
        ]),
        ..Default::default()
    };
    assert_eq!(resolve_base_url(&pdef), "https://cn.example/v1");

    unsafe {
        std::env::set_var("HARN_TEST_BASE_URL", " 'https://override.example/v1' ");
    }
    assert_eq!(resolve_base_url(&pdef), "https://override.example/v1");
}

#[test]
fn test_default_config_roundtrip() {
    let config = default_config();
    assert!(!config.providers.is_empty());
    assert!(!config.inference_rules.is_empty());
    // Tier is now declared on each model row; tier_rules is allowed
    // to be empty (the rule table is a legacy fallback only).
    assert_eq!(config.tier_defaults.default, "mid");
    // At least the new open-weight frontiers should have explicit tiers.
    let frontiers = config
        .models
        .iter()
        .filter(|(_, m)| m.tier.as_deref() == Some("frontier"))
        .count();
    assert!(
        frontiers >= 4,
        "expected at least 4 frontier-tagged models, got {frontiers}"
    );
}

#[test]
fn test_local_ollama_catalog_metadata() {
    reset_overrides();

    let devstral =
        model_catalog_entry("devstral-small-2:24b").expect("devstral-small-2 catalog entry");
    assert_eq!(devstral.context_window, 262_144);
    assert!(!devstral.capabilities.iter().any(|cap| cap == "vision"));

    let gemma4 = model_catalog_entry("gemma4:26b").expect("gemma4 catalog entry");
    assert_eq!(gemma4.context_window, 262_144);
    assert!(gemma4.capabilities.iter().any(|cap| cap == "vision"));
}

#[test]
fn local_gemma4_source_tags_match_structured_capability_tags() {
    reset_overrides();
    let config = default_config();
    for id in [
        "gemma-4-e2b-it",
        "gemma-4-e4b-it",
        "gemma-4-12b-it",
        "gemma-4-26b-a4b-it",
        "gemma-4-31b-it",
    ] {
        let source = config
            .models
            .get(id)
            .unwrap_or_else(|| panic!("{id} should be in the embedded catalog"));
        let derived = effective_model_capability_tags(&source.provider, id);
        assert_eq!(
            source.capabilities, derived,
            "{}/{} source capabilities must match derived capability_tags",
            source.provider, id
        );
    }
}

#[test]
fn capability_tags_include_structured_capability_flags() {
    let caps = crate::llm::capabilities::Capabilities {
        native_tools: true,
        tool_search: vec!["web".to_string()],
        vision_supported: true,
        audio: true,
        pdf: true,
        video: true,
        files_api_supported: true,
        batch_api: true,
        prompt_caching: true,
        thinking_modes: vec!["enabled".to_string()],
        structured_output: Some("native".to_string()),
        ..Default::default()
    };

    assert_eq!(
        capability_tags_from_capabilities(&caps),
        vec![
            "streaming",
            "tools",
            "tool_search",
            "vision",
            "audio",
            "pdf",
            "video",
            "files",
            "batch",
            "prompt_caching",
            "thinking",
            "structured_output",
        ]
    );
}

#[test]
fn test_external_config_overlays_default_catalog() {
    let mut config = default_config();
    let mut overlay = ProvidersConfig {
        default_provider: Some("ollama".to_string()),
        ..Default::default()
    };
    overlay.providers.insert(
        "custom".to_string(),
        ProviderDef {
            base_url: "https://llm.example.test/v1".to_string(),
            chat_endpoint: "/chat/completions".to_string(),
            ..Default::default()
        },
    );

    config.merge_from(&overlay);

    assert_eq!(config.default_provider.as_deref(), Some("ollama"));
    assert!(config.providers.contains_key("custom"));
    assert!(config.providers.contains_key("anthropic"));
    assert!(config.providers.contains_key("ollama"));
}

#[test]
fn test_model_params_empty() {
    let params = model_params("claude-sonnet-4-20250514");
    assert!(params.is_empty());
}

#[test]
fn test_user_overrides_add_provider_and_alias() {
    reset_overrides();
    let mut overlay = ProvidersConfig::default();
    overlay.providers.insert(
        "acme".to_string(),
        ProviderDef {
            base_url: "https://llm.acme.test/v1".to_string(),
            chat_endpoint: "/chat/completions".to_string(),
            ..Default::default()
        },
    );
    overlay.aliases.insert(
        "acme-fast".to_string(),
        AliasDef {
            id: "acme/model-fast".to_string(),
            provider: "acme".to_string(),
            tool_format: Some("native".to_string()),
        },
    );
    set_user_overrides(Some(overlay));

    let (model, provider) = resolve_model("acme-fast");
    assert_eq!(model, "acme/model-fast");
    assert_eq!(provider.as_deref(), Some("acme"));
    assert!(provider_names().contains(&"acme".to_string()));
    assert_eq!(
        provider_config("acme").map(|provider| provider.base_url),
        Some("https://llm.acme.test/v1".to_string())
    );

    reset_overrides();
}

#[test]
fn test_default_tool_format_uses_capability_matrix() {
    reset_overrides();

    assert_eq!(
        default_tool_format("qwen3.6-35b-a3b-ud-q4-k-xl", "llamacpp"),
        "native"
    );
    // devstral dropped its stale heredoc `text` pin (it has no reserved-token
    // constraint, so there was no structural reason to stay on heredoc) and
    // now inherits the global `json` text-channel default. Heredoc is still
    // reachable via an explicit `preferred_tool_format = "text"` pin.
    assert_eq!(
        default_tool_format("devstral-small-2:24b", "ollama"),
        "json"
    );
    // vLLM/SGLang-served Gemma 4 exposes OpenAI-compatible function calling,
    // so the local route declares native tools (matching every hosted gemma-4
    // sibling) rather than degrading to a text tool format.
    assert_eq!(default_tool_format("gemma-4-26b-a4b-it", "local"), "native");
    // deepseek-v3.2 and qwen3-coder both pin `text` in the capability
    // matrix, so they keep heredoc rather than inheriting the json default.
    assert_eq!(
        default_tool_format("deepseek/deepseek-v3.2", "openrouter"),
        "text"
    );
    assert_eq!(
        default_tool_format("qwen/qwen3-coder-flash", "openrouter"),
        "text"
    );
    assert_eq!(
        default_tool_format("qwen/qwen3.6-flash", "openrouter"),
        "native"
    );
    // GLM is native since the 2026-08-15 re-probe; DeepInfra's GLM-5.2 kept a
    // pin and steers to fenced JSON rather than heredoc.
    assert_eq!(default_tool_format("z-ai/glm-5.2", "openrouter"), "native");
    assert_eq!(default_tool_format("zai-org/GLM-5.2", "deepinfra"), "json");
    // GPT-OSS tool defaults are provider-specific: aggregate OpenRouter and
    // Fireworks use Harn's heredoc text tools, as does DeepInfra — its
    // native Harmony channel drops tool calls into the private reasoning
    // channel (footgun), so it is pinned to text. Native-reliable hosts
    // (Cerebras, Groq) stay on provider-native tool calls.
    assert_eq!(
        default_tool_format("openai/gpt-oss-120b", "openrouter"),
        "text"
    );
    assert_eq!(
        default_tool_format("accounts/fireworks/models/gpt-oss-120b", "fireworks"),
        "text"
    );
    assert_eq!(default_tool_format("gpt-oss-120b", "cerebras"), "native");
    assert_eq!(
        default_tool_format("openai/gpt-oss-120b", "deepinfra"),
        "text"
    );
    assert_eq!(default_tool_format("openai/gpt-oss-120b", "groq"), "native");
}

#[test]
fn test_default_tool_format_unpinned_text_channel_is_json() {
    reset_overrides();

    // GLOBAL DEFAULT FLIP: a model with no capability-matrix pin and no
    // native tool support resolves to fenced-json (`json`), not heredoc
    // (`text`). This is the behavior change — an unknown text-channel model
    // gets the delimiter-safe default. (Native-capable unknowns still get
    // `native`; pinned models still honor their pin, covered above.)
    assert_eq!(default_tool_format("mystery-model-xyz", "ollama"), "json");
}

#[test]
fn test_claude_family_defaults_native_without_host_pin() {
    reset_overrides();

    // Unpinned claude-family routes on first-class tool-calling providers
    // resolve `native` from the capability matrix alone — no host alias
    // pin required. The openrouter rows exercise the family-level
    // catch-all: a dated slug, an unparseable version segment, and a new
    // family name have no versioned rule and previously fell through to
    // the global text-channel `json` default.
    for (model, provider) in [
        ("claude-sonnet-4-6", "anthropic"),
        ("claude-sonnet-5", "anthropic"),
        ("anthropic/claude-nova-1", "anthropic"),
        ("anthropic/claude-sonnet-4.6", "openrouter"),
        ("anthropic/claude-sonnet-5", "openrouter"),
        ("anthropic/claude-opus-4-5-20251101", "openrouter"),
        ("anthropic/claude-sonnet-next", "openrouter"),
        ("anthropic/claude-nova-1", "openrouter"),
        ("anthropic.claude-sonnet-4-6", "bedrock"),
    ] {
        assert_eq!(
            default_tool_format(model, provider),
            "native",
            "{provider}:{model} must default native without a host pin"
        );
    }

    // An unpinned host alias resolves native end-to-end through
    // `resolve_model_info` (alias -> id -> capability matrix -> dialect
    // guard) — the exact seam hosts consume via `llm_resolve_model`.
    let overlay = parse_config_toml(
        "[aliases.probe-sonnet]\nid = \"claude-sonnet-4-6\"\nprovider = \"anthropic\"\n",
    )
    .expect("overlay parses");
    set_user_overrides(Some(overlay));
    let resolved = resolve_model_info("probe-sonnet");
    assert_eq!(resolved.provider, "anthropic");
    assert_eq!(
        resolved.tool_format, "native",
        "an unpinned claude alias must inherit the family-level native default"
    );
    clear_user_overrides();

    // An explicit host pin still wins over the family default: a
    // text-channel `json` pin on a native-capable claude route survives
    // resolution (the dialect guard only corrects known-broken combos).
    let overlay = parse_config_toml(
            "[aliases.probe-sonnet-json]\nid = \"claude-sonnet-4-6\"\nprovider = \"anthropic\"\ntool_format = \"json\"\n",
        )
        .expect("overlay parses");
    set_user_overrides(Some(overlay));
    let pinned = resolve_model_info("probe-sonnet-json");
    assert_eq!(
        pinned.tool_format, "json",
        "an explicit host pin must win over the claude family default"
    );
    clear_user_overrides();

    // Non-claude models keep the global text-channel `json` default —
    // the catch-all is family-scoped, not a provider-wide flip.
    assert_eq!(
        default_tool_format("mystery-model-xyz", "openrouter"),
        "json"
    );
}

#[test]
fn test_user_overrides_add_model_catalog_pricing_and_qc_defaults() {
    reset_overrides();
    let mut overlay = ProvidersConfig::default();
    overlay.models.insert(
        "acme/model-fast".to_string(),
        ModelDef {
            name: "Acme Fast".to_string(),
            display_name: None,
            blurb: None,
            provider: "acme".to_string(),
            context_window: 65_536,
            logical_model: None,
            equivalence_group: None,
            served_variant: None,
            wire_model: None,
            api_dialect: None,
            rate_limits: None,
            performance: None,
            architecture: None,
            local_memory: None,
            runtime_context_window: None,
            stream_timeout: Some(42.0),
            capabilities: vec!["tools".to_string(), "streaming".to_string()],
            pricing: Some(ModelPricing {
                input_per_mtok: 1.25,
                output_per_mtok: 2.5,
                cache_read_per_mtok: Some(0.25),
                cache_write_per_mtok: None,
                input_token_bands: Vec::new(),
            }),
            deprecated: false,
            deprecation_note: None,
            superseded_by: None,
            serving_tiers: Vec::new(),
            quality_tags: Vec::new(),
            availability: ModelAvailability::default(),
            tier: None,
            open_weight: None,
            strengths: Vec::new(),
            benchmarks: std::collections::BTreeMap::new(),
            family: None,
            lineage: None,
            complementary_with: Vec::new(),
            avoid_as_reviewer_for: Vec::new(),
        },
    );
    overlay
        .qc_defaults
        .insert("acme".to_string(), "acme/model-cheap".to_string());
    set_user_overrides(Some(overlay));

    let entry = model_catalog_entry("acme/model-fast").expect("catalog entry");
    assert_eq!(entry.context_window, 65_536);
    assert_eq!(
        entry.capabilities,
        vec!["streaming".to_string(), "tools".to_string()]
    );
    assert_eq!(
        entry.pricing.as_ref().map(|pricing| pricing.input_per_mtok),
        Some(1.25)
    );
    assert_eq!(
        pricing_per_1k_for("acme", "acme/model-fast"),
        Some((0.00125, 0.0025))
    );
    assert_eq!(
        qc_default_model("acme").as_deref(),
        Some("acme/model-cheap")
    );

    reset_overrides();
}

#[test]
fn test_user_overrides_prepend_inference_rules() {
    reset_overrides();
    let mut overlay = ProvidersConfig::default();
    overlay.inference_rules.push(InferenceRule {
        pattern: Some("internal-*".to_string()),
        contains: None,
        exact: None,
        provider: "openai".to_string(),
    });
    set_user_overrides(Some(overlay));

    assert_eq!(infer_provider("internal-foo"), "openai");

    reset_overrides();
}

mod diagnostics;
mod embedded_catalog;
mod overlays;
mod provider_prefix;
mod tool_protocol;
