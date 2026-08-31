use super::{
    effective_config, normalize_model_id, resolve_model_info, resolve_model_request,
    resolve_model_request_for_active_call, resolve_model_request_with_config, AliasDef,
    ModelResolutionError, ProviderResolutionScope, MODEL_CATALOG_VERSION,
};

#[test]
fn registered_provider_selector_normalizes_to_native_model_id() {
    let model = resolve_model_info("openai:o3");
    assert_eq!(model.provider, "openai");
    assert_eq!(model.id, "o3");
    assert_eq!(normalize_model_id("mock:o3"), "o3");
    assert_eq!(
        normalize_model_id("ollama:qwen3.2:latest"),
        "qwen3.2:latest",
        "only the first selector colon is transport syntax"
    );
}

#[test]
fn qualified_model_is_confined_to_its_requested_provider() {
    let resolution = resolve_model_request("openai:gpt-5.6-sol", None)
        .expect("the qualified current catalog route resolves");
    assert_eq!(resolution.requested_model, "openai:gpt-5.6-sol");
    assert_eq!(resolution.alias_chain, Vec::<String>::new());
    assert_eq!(resolution.resolved_provider, "openai");
    assert_eq!(resolution.resolved_model, "gpt-5.6-sol");
    assert_eq!(resolution.catalog_version, MODEL_CATALOG_VERSION);
}

#[test]
fn qualified_model_cannot_cross_to_catalogued_provider() {
    let error = resolve_model_request("ollama:gpt-5.6-sol", None)
        .expect_err("a qualified selector cannot cross providers");
    assert!(matches!(
        error,
        ModelResolutionError::ProviderModelMismatch {
            ref provider,
            ref model,
            ref catalog_provider,
            ..
        } if provider == "ollama" && model == "gpt-5.6-sol" && catalog_provider == "openai"
    ));
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("catalogued for provider 'openai'"));
    assert!(diagnostic.contains(MODEL_CATALOG_VERSION));
}

#[test]
fn qualified_model_cannot_cross_a_builtin_model_family() {
    let error = resolve_model_request("openai:claude-unreleased-private", None)
        .expect_err("a qualified selector cannot contradict a known model family");
    assert!(matches!(
        error,
        ModelResolutionError::ProviderModelMismatch {
            ref provider,
            ref catalog_provider,
            ..
        } if provider == "openai" && catalog_provider == "anthropic"
    ));
}

#[test]
fn misspelled_provider_fails_with_provider_suggestion() {
    let error = resolve_model_request("opneai:gpt-5.6-sol", None)
        .expect_err("a provider typo must not become an Ollama model tag");
    assert!(matches!(
        error,
        ModelResolutionError::UnknownProvider { .. }
    ));
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("opneai"));
    assert!(diagnostic.contains("openai"));
    assert!(diagnostic.contains(MODEL_CATALOG_VERSION));
}

#[test]
fn alias_drift_fails_with_catalog_version_and_near_match() {
    let error = resolve_model_request("gpt-5.6-slo", None)
        .expect_err("a near-miss alias must not silently pass through");
    assert!(matches!(error, ModelResolutionError::UnknownModel { .. }));
    let diagnostic = error.to_string();
    assert!(diagnostic.contains(MODEL_CATALOG_VERSION));
    assert!(diagnostic.contains("gpt-5.6-sol"));
}

#[test]
fn explicit_provider_keeps_private_model_ids_extensible() {
    let resolution = resolve_model_request("new-private-model", Some("openai"))
        .expect("an explicit provider may serve a private or newly released id");
    assert_eq!(resolution.resolved_provider, "openai");
    assert_eq!(resolution.resolved_model, "new-private-model");
}

#[test]
fn runtime_adapter_can_proxy_a_catalogued_model() {
    let resolution = resolve_model_request("claude-sonnet-4-6", Some("mock"))
        .expect("a runtime adapter may proxy an upstream model identity");
    assert_eq!(resolution.resolved_provider, "mock");
    assert_eq!(resolution.resolved_model, "claude-sonnet-4-6");
}

#[test]
fn active_fixture_is_a_scoped_open_world_transport_adapter() {
    struct ResetMockOnDrop;
    impl Drop for ResetMockOnDrop {
        fn drop(&mut self) {
            crate::llm::mock::reset_llm_mock_state();
        }
    }

    crate::llm::mock::reset_llm_mock_state();
    let _reset = ResetMockOnDrop;
    let strict = resolve_model_request("m", Some("fixture-provider"))
        .expect_err("catalog resolution must reject an unregistered provider");
    assert!(matches!(
        strict,
        ModelResolutionError::UnknownProvider { .. }
    ));

    let fixture = crate::llm::parse_llm_mock_value(&serde_json::json!({"text": "fixture"}))
        .expect("valid inline fixture");
    crate::llm::mock::push_llm_mock(fixture);
    let active = resolve_model_request_for_active_call("m", Some("fixture-provider"))
        .expect("the active fixture owns its scripted provider identity");
    let colon_model =
        resolve_model_request_for_active_call("qwen3.2:latest", Some("fixture-provider"))
            .expect("an open-world fixture provider must not capture model-id colons as syntax");
    assert_eq!(active.resolved_provider, "fixture-provider");
    assert_eq!(active.resolved_model, "m");
    assert_eq!(colon_model.resolved_provider, "fixture-provider");
    assert_eq!(colon_model.resolved_model, "qwen3.2:latest");
}

#[test]
fn configured_proxy_can_serve_an_upstream_model_identity() {
    let mut config = (*effective_config()).clone();
    let proxy = config
        .providers
        .get("openai")
        .expect("the embedded OpenAI provider exists")
        .clone();
    config.providers.insert("my-proxy".to_string(), proxy);
    config.aliases.insert(
        "proxied-sonnet".to_string(),
        AliasDef {
            id: "claude-sonnet-4-6".to_string(),
            provider: "my-proxy".to_string(),
            tool_format: None,
        },
    );

    let resolution = resolve_model_request_with_config(
        &config,
        "proxied-sonnet",
        None,
        ProviderResolutionScope::Catalog,
    )
    .expect("a configured proxy may serve an upstream model identity");
    assert_eq!(resolution.resolved_provider, "my-proxy");
    assert_eq!(resolution.resolved_model, "claude-sonnet-4-6");
}

#[test]
fn explicit_local_provider_remains_the_generic_openai_compatible_adapter() {
    let resolution = resolve_model_request("qwen3.2:latest", Some("local"))
        .expect("an explicit local provider is not the local: selector shorthand");
    assert_eq!(resolution.resolved_provider, "local");
    assert_eq!(resolution.resolved_model, "qwen3.2:latest");
}

#[test]
fn explicit_hugging_face_shorthand_still_resolves_to_the_catalog_provider() {
    let resolution = resolve_model_request("Qwen/Qwen3-Coder-480B-A35B-Instruct", Some("hf"))
        .expect("the documented hf provider shorthand remains accepted");
    assert_eq!(resolution.resolved_provider, "huggingface");
    assert_eq!(
        resolution.resolved_model,
        "Qwen/Qwen3-Coder-480B-A35B-Instruct"
    );
}

#[test]
fn explicit_provider_preserves_a_provider_native_colon_model_id() {
    let resolution = resolve_model_request("llava:latest", Some("ollama"))
        .expect("a provider-native tag is not an unknown provider qualifier");
    assert_eq!(resolution.resolved_provider, "ollama");
    assert_eq!(resolution.resolved_model, "llava:latest");

    let typo = resolve_model_request("opneai:gpt-5.6-sol", Some("openai"))
        .expect_err("a typo before a catalogued model suffix must remain visible");
    assert!(matches!(typo, ModelResolutionError::UnknownProvider { .. }));
}

#[test]
fn same_name_provider_alias_is_a_terminal_annotation() {
    let resolution = resolve_model_request("deepseek-v4-flash", Some("deepseek"))
        .expect("same-name aliases attach provider identity without recursing");
    assert!(resolution.alias_chain.is_empty());
    assert_eq!(resolution.resolved_provider, "deepseek");
    assert_eq!(resolution.resolved_model, "deepseek-v4-flash");
}

#[test]
fn alias_resolution_records_the_chain() {
    let resolution =
        resolve_model_request("gpt-5.6", None).expect("the current family alias resolves");
    assert_eq!(resolution.alias_chain, ["gpt-5.6"]);
    assert_eq!(resolution.resolved_provider, "openai");
    assert_eq!(resolution.resolved_model, "gpt-5.6-sol");
}

#[test]
fn alias_provider_cannot_contradict_its_catalogued_model() {
    let mut config = (*effective_config()).clone();
    config.aliases.insert(
        "cross-provider-alias".to_string(),
        AliasDef {
            id: "gpt-5.6-sol".to_string(),
            provider: "ollama".to_string(),
            tool_format: None,
        },
    );

    let error = resolve_model_request_with_config(
        &config,
        "cross-provider-alias",
        None,
        ProviderResolutionScope::Catalog,
    )
    .expect_err("an alias cannot lie about the provider of a catalogued model");
    assert!(matches!(
        error,
        ModelResolutionError::ProviderModelMismatch {
            ref provider,
            ref catalog_provider,
            ..
        } if provider == "ollama" && catalog_provider == "openai"
    ));
}

#[test]
fn grok_code_aliases_resolve_through_live_resolver() {
    for selector in ["grok-code", "grok-code-fast", "grok-code-fast-1"] {
        let model = resolve_model_info(selector);
        assert_eq!(
            (
                model.id.as_str(),
                model.provider.as_str(),
                model.alias.as_deref(),
                model.tool_format.as_str(),
            ),
            ("grok-build-0.1", "xai", Some(selector), "native"),
            "selector: {selector}",
        );
    }
}

#[test]
fn huggingface_qwen3_coder_aliases_resolve_through_live_resolver() {
    for selector in ["huggingface-qwen3-coder", "hf-qwen3-coder"] {
        let model = resolve_model_info(selector);
        assert_eq!(
            (
                model.id.as_str(),
                model.provider.as_str(),
                model.alias.as_deref(),
                model.tool_format.as_str(),
                model.tier.as_str(),
            ),
            (
                "Qwen/Qwen3-Coder-480B-A35B-Instruct",
                "huggingface",
                Some(selector),
                "native",
                "frontier",
            ),
            "selector: {selector}",
        );
    }
}
