use super::{
    effective_config, normalize_model_id, resolve_model_info, resolve_model_request,
    resolve_model_request_with_config, AliasDef, ModelResolutionError, MODEL_CATALOG_VERSION,
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

    let error = resolve_model_request_with_config(&config, "cross-provider-alias", None)
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
