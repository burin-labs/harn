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
fn test_openrouter_inference_requires_one_slash() {
    let _guard = crate::llm::env_guard();
    let _env = crate::test_env::test_env_guard();

    assert_eq!(infer_provider("org/model"), "openrouter");
    assert_eq!(infer_provider("org/team/model"), "anthropic");
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
            sunset_date: None,
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
            completion_review: None,
            released: None,
            row_kind: None,
            current_snapshot: None,
            embedding_dim: None,
            embedding_max_tokens: None,
        },
    );
    set_user_overrides(Some(overlay));

    assert_eq!(infer_provider("gpt-4o"), "openrouter");

    reset_overrides();
}

#[test]
fn test_resolve_model_unknown_alias() {
    let (id, provider) = resolve_model("gpt-4o");
    assert_eq!(id, "gpt-4o");
    assert!(provider.is_none());
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
fn host_and_user_provider_files_layer_with_user_precedence() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let host_path = directory.path().join("host-providers.toml");
    let user_path = directory.path().join("user-providers.toml");
    std::fs::write(
        &host_path,
        r#"
            default_provider = "ollama"
            [aliases.host-only]
            id = "host/model"
            provider = "ollama"
            [aliases.collision]
            id = "host/loses"
            provider = "ollama"
            "#,
    )
    .expect("write host overlay");
    std::fs::write(
        &user_path,
        r#"
            default_provider = "openai"
            [aliases.user-only]
            id = "user/model"
            provider = "openai"
            [aliases.collision]
            id = "user/wins"
            provider = "openai"
            "#,
    )
    .expect("write user overlay");

    let (config, loaded_paths) = super::loading::load_external_config_layers(
        default_config(),
        host_path.to_str(),
        user_path.to_str(),
        None,
        false,
    );

    assert!(config.aliases.contains_key("host-only"));
    assert!(config.aliases.contains_key("user-only"));
    assert_eq!(config.aliases["collision"].id, "user/wins");
    assert_eq!(config.default_provider.as_deref(), Some("openai"));
    assert_eq!(loaded_paths, vec![host_path, user_path]);
}

#[test]
fn explicit_user_provider_file_preserves_home_fallback_contract() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let explicit_path = directory.path().join("explicit-providers.toml");
    let missing_path = directory.path().join("missing-providers.toml");
    let home_path = directory.path().join("home-providers.toml");
    std::fs::write(
        &explicit_path,
        "[aliases.explicit-only]\nid = \"explicit/model\"\nprovider = \"openai\"\n",
    )
    .expect("write explicit user overlay");
    std::fs::write(
        &home_path,
        "[aliases.home-only]\nid = \"home/model\"\nprovider = \"ollama\"\n",
    )
    .expect("write home user overlay");

    let (explicit, explicit_paths) = super::loading::load_external_config_layers(
        default_config(),
        None,
        explicit_path.to_str(),
        home_path.to_str(),
        false,
    );
    assert!(explicit.aliases.contains_key("explicit-only"));
    assert!(!explicit.aliases.contains_key("home-only"));
    assert_eq!(explicit_paths, vec![explicit_path]);

    let (fallback, fallback_paths) = super::loading::load_external_config_layers(
        default_config(),
        None,
        missing_path.to_str(),
        home_path.to_str(),
        false,
    );
    assert!(fallback.aliases.contains_key("home-only"));
    assert_eq!(fallback_paths, vec![home_path]);
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
fn cerebras_gemma_4_catalog_row_preserves_public_route_metadata() {
    let model = model_catalog_entry("gemma-4-31b")
        .expect("Cerebras Gemma 4's public serverless route must be catalogued");

    assert_eq!(model.provider, "cerebras");
    assert_eq!(model.context_window, 131_072);
    assert_eq!(
        model.capabilities,
        vec![
            "streaming".to_string(),
            "tools".to_string(),
            "vision".to_string(),
            "thinking".to_string(),
            "structured_output".to_string(),
        ]
    );
    let pricing = model
        .pricing
        .expect("Cerebras Gemma 4's public token rates must be catalogued");
    assert_eq!(pricing.input_per_mtok, 0.99);
    assert_eq!(pricing.output_per_mtok, 1.49);

    let capabilities = crate::llm::capabilities::lookup("cerebras", "gemma-4-31b");
    assert!(capabilities.native_tools);
    assert!(capabilities.vision_supported);
    assert_eq!(capabilities.structured_output.as_deref(), Some("native"));
    assert_eq!(
        capabilities.preferred_tool_format.as_deref(),
        Some("native")
    );
    assert_eq!(capabilities.thinking_modes, vec!["enabled"]);
}

#[test]
fn groq_qwen_3_8_catalog_row_preserves_public_route_metadata() {
    let model = model_catalog_entry("qwen/qwen3.8-27b")
        .expect("Groq Qwen 3.8's public preview route must be catalogued");

    assert_eq!(model.provider, "groq");
    assert_eq!(model.context_window, 131_042);
    assert_eq!(model.wire_model.as_deref(), Some("qwen/qwen3.8-27b"));
    assert_eq!(model.open_weight, Some(true));
    for capability in ["tools", "vision", "streaming", "thinking"] {
        assert!(
            model.capabilities.iter().any(|value| value == capability),
            "Groq Qwen 3.8 must expose {capability}"
        );
    }
    let pricing = model
        .pricing
        .expect("Groq Qwen 3.8's public token rates must be catalogued");
    assert_eq!(pricing.input_per_mtok, 0.80);
    assert_eq!(pricing.output_per_mtok, 4.00);

    let capabilities = crate::llm::capabilities::lookup("groq", "qwen/qwen3.8-27b");
    assert!(capabilities.native_tools);
    assert!(capabilities.tools_exclude_response_format);
    assert!(capabilities.vision_supported);
    assert_eq!(capabilities.structured_output.as_deref(), Some("native"));
    assert_eq!(capabilities.thinking_modes, vec!["effort"]);
    assert_eq!(
        capabilities.reasoning_effort_levels,
        vec!["none", "low", "medium", "high"]
    );
    assert!(capabilities.reasoning_none_supported);
    assert!(capabilities.presence_penalty_supported);
    assert!(!capabilities.top_k_supported);
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
                promotions: Vec::new(),
            }),
            deprecated: false,
            deprecation_note: None,
            sunset_date: None,
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
            completion_review: None,
            released: None,
            row_kind: None,
            current_snapshot: None,
            embedding_dim: None,
            embedding_max_tokens: None,
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

#[test]
fn embeddings_endpoint_and_model_dims_parse_from_toml() {
    let config = parse_config_toml(
        r#"
[providers.openai]
base_url = "https://api.openai.com/v1"
chat_endpoint = "/chat/completions"
embeddings_endpoint = "/embeddings"
features = ["embeddings"]

[models."text-embedding-3-small"]
name = "Text Embedding 3 Small"
provider = "openai"
context_window = 8191
capabilities = ["embeddings"]
embedding_dim = 1536
embedding_max_tokens = 8191
"#,
    )
    .expect("config parses");
    let provider = config.providers.get("openai").expect("openai provider");
    assert_eq!(provider.embeddings_endpoint.as_deref(), Some("/embeddings"));
    assert!(provider
        .features
        .iter()
        .any(|feature| feature == "embeddings"));
    let model = config
        .models
        .get("text-embedding-3-small")
        .expect("embedding model");
    assert_eq!(model.embedding_dim, Some(1536));
    assert_eq!(model.embedding_max_tokens, Some(8191));
}

#[test]
fn retired_groq_llama_models_are_absent_from_bundled_catalog() {
    reset_overrides();

    for model in ["llama-3.1-8b-instant", "llama-3.3-70b-versatile"] {
        assert!(
            model_catalog_entry(model).is_none(),
            "retired Groq model `{model}` must not be selected from the bundled catalog"
        );
    }

    let active = model_catalog_entry("qwen/qwen3.6-27b")
        .expect("the current Groq catalog route must remain available");
    assert_eq!(active.provider, "groq");
}

mod diagnostics;
mod embedded_catalog;
mod overlays;
mod provider_prefix;
mod tool_protocol;
