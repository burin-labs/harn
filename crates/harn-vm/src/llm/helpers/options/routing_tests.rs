use super::extract::*;
use super::*;
use crate::value::VmDictExt;

use crate::llm_config::{
    AliasDef, AuthEnv, ModelAvailability, ModelDef, ProviderDef, ProvidersConfig, TierRule,
};

fn install_test_routes() {
    let mut overlay = ProvidersConfig::default();
    overlay.providers.insert(
        "cheap".to_string(),
        ProviderDef {
            base_url: "https://cheap.example/v1".to_string(),
            auth_style: "none".to_string(),
            auth_env: AuthEnv::None,
            chat_endpoint: "/chat/completions".to_string(),
            cost_per_1k_in: Some(0.0),
            cost_per_1k_out: Some(0.0),
            latency_p50_ms: Some(2200),
            ..Default::default()
        },
    );
    overlay.providers.insert(
        "fast".to_string(),
        ProviderDef {
            base_url: "https://fast.example/v1".to_string(),
            auth_style: "none".to_string(),
            auth_env: AuthEnv::None,
            chat_endpoint: "/chat/completions".to_string(),
            cost_per_1k_in: Some(0.01),
            cost_per_1k_out: Some(0.02),
            latency_p50_ms: Some(250),
            ..Default::default()
        },
    );
    overlay.aliases.insert(
        "cheap-mid".to_string(),
        AliasDef {
            id: "cheap-mid-model".to_string(),
            provider: "cheap".to_string(),
            tool_format: None,
        },
    );
    overlay.aliases.insert(
        "fast-mid".to_string(),
        AliasDef {
            id: "fast-mid-model".to_string(),
            provider: "fast".to_string(),
            tool_format: None,
        },
    );
    overlay.tier_rules.push(TierRule {
        exact: Some("cheap-mid-model".to_string()),
        pattern: None,
        contains: None,
        tier: "mid".to_string(),
    });
    overlay.tier_rules.push(TierRule {
        exact: Some("fast-mid-model".to_string()),
        pattern: None,
        contains: None,
        tier: "mid".to_string(),
    });
    crate::llm_config::set_user_overrides(Some(overlay));
    super::super::reset_provider_key_cache();
}

fn test_provider(url: &str) -> ProviderDef {
    ProviderDef {
        base_url: url.to_string(),
        auth_style: "none".to_string(),
        auth_env: AuthEnv::None,
        chat_endpoint: "/chat/completions".to_string(),
        cost_per_1k_in: Some(0.0),
        cost_per_1k_out: Some(0.0),
        latency_p50_ms: Some(1000),
        ..Default::default()
    }
}

fn test_equivalent_model(provider: &str, group: &str) -> ModelDef {
    ModelDef {
        name: format!("{provider} equivalent model"),
        provider: provider.to_string(),
        context_window: 32_000,
        logical_model: None,
        equivalence_group: Some(group.to_string()),
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
        fast_mode: None,
        quality_tags: Vec::new(),
        availability: ModelAvailability::Serverless,
        tier: Some("mid".to_string()),
        open_weight: Some(true),
        strengths: Vec::new(),
        benchmarks: std::collections::BTreeMap::new(),
        family: Some("test-equivalent-family".to_string()),
        lineage: None,
        complementary_with: Vec::new(),
        avoid_as_reviewer_for: Vec::new(),
    }
}

fn install_equivalent_routes() {
    let mut overlay = ProvidersConfig::default();
    overlay.providers.insert(
        "primary".to_string(),
        test_provider("https://primary.example/v1"),
    );
    overlay.providers.insert(
        "backup-a".to_string(),
        test_provider("https://backup-a.example/v1"),
    );
    overlay.providers.insert(
        "backup-b".to_string(),
        test_provider("https://backup-b.example/v1"),
    );
    overlay.models.insert(
        "primary-model".to_string(),
        test_equivalent_model("primary", "test-equivalent-model"),
    );
    overlay.models.insert(
        "backup-a-model".to_string(),
        test_equivalent_model("backup-a", "test-equivalent-model"),
    );
    overlay.models.insert(
        "backup-b-model".to_string(),
        test_equivalent_model("backup-b", "test-equivalent-model"),
    );
    overlay.models.insert(
        "same-provider-model".to_string(),
        test_equivalent_model("primary", "test-equivalent-model"),
    );
    crate::llm_config::set_user_overrides(Some(overlay));
    super::super::reset_provider_key_cache();
}

fn extract_with_policy(policy: &str) -> crate::llm::api::LlmCallOptions {
    let mut options = crate::value::DictMap::new();
    options.put_str("route_policy", policy);
    options.insert(
        crate::value::intern_key("fallback_chain"),
        VmValue::List(std::sync::Arc::new(vec![VmValue::String(
            arcstr::ArcStr::from("fast".to_string()),
        )])),
    );
    extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("options")
}

#[test]
fn cheapest_over_quality_selects_lowest_cost_available_candidate() {
    install_test_routes();
    let opts = extract_with_policy("cheapest_over_quality(mid)");
    assert_eq!(opts.provider, "cheap");
    assert_eq!(opts.model, "cheap-mid-model");
    assert_eq!(opts.fallback_chain, vec!["fast".to_string()]);
    let decision = opts.routing_decision.expect("routing decision");
    assert!(decision.alternatives.iter().any(|alt| alt.selected));
    assert!(decision
        .alternatives
        .iter()
        .any(|alt| alt.provider == "fast"));
    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();
}

fn extract_with_options(
    opts: crate::value::DictMap,
) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(opts),
    ])
}

fn model_role_options(role: &str) -> crate::value::DictMap {
    crate::value::DictMap::from_iter([(
        crate::value::intern_key("model_role"),
        VmValue::String(arcstr::ArcStr::from(role.to_string())),
    )])
}

fn clear_merge_role_env() {
    std::env::remove_var("HARN_LLM_MERGE_PROVIDER");
    std::env::remove_var("HARN_LLM_MERGE_MODEL");
    std::env::remove_var("HARN_LLM_MERGE_ROUTE_POLICY");
    std::env::remove_var("HARN_LLM_ROLE_MERGE_PROVIDER");
    std::env::remove_var("HARN_LLM_ROLE_MERGE_MODEL");
    std::env::remove_var("HARN_LLM_ROLE_MERGE_ROUTE_POLICY");
    std::env::remove_var("HARN_LLM_FAST_APPLY_PROVIDER");
    std::env::remove_var("HARN_LLM_FAST_APPLY_MODEL");
    std::env::remove_var("HARN_LLM_FAST_APPLY_ROUTE_POLICY");
    std::env::remove_var("HARN_LLM_ROLE_FAST_APPLY_PROVIDER");
    std::env::remove_var("HARN_LLM_ROLE_FAST_APPLY_MODEL");
    std::env::remove_var("HARN_LLM_ROLE_FAST_APPLY_ROUTE_POLICY");
}

#[test]
fn model_role_defaults_fill_missing_llm_options() {
    let _guard = crate::llm::env_guard();
    clear_merge_role_env();
    let mut overlay = ProvidersConfig::default();
    overlay.model_roles.insert(
        "merge".to_string(),
        std::collections::BTreeMap::from_iter([
            (
                "provider".to_string(),
                toml::Value::String("mock".to_string()),
            ),
            (
                "model".to_string(),
                toml::Value::String("mock-merge".to_string()),
            ),
            ("max_tokens".to_string(), toml::Value::Integer(4096)),
            ("temperature".to_string(), toml::Value::Float(0.0)),
        ]),
    );
    crate::llm_config::set_user_overrides(Some(overlay));
    super::super::reset_provider_key_cache();

    let opts = extract_with_options(model_role_options("merge")).expect("options");

    assert_eq!(opts.provider, "mock");
    assert_eq!(opts.model, "mock-merge");
    assert_eq!(opts.max_tokens, 4096);
    assert_eq!(opts.temperature, Some(0.0));

    crate::llm_config::clear_user_overrides();
    clear_merge_role_env();
    super::super::reset_provider_key_cache();
}

#[test]
fn explicit_options_win_over_model_role_defaults() {
    let _guard = crate::llm::env_guard();
    clear_merge_role_env();
    let mut overlay = ProvidersConfig::default();
    overlay.model_roles.insert(
        "merge".to_string(),
        std::collections::BTreeMap::from_iter([
            (
                "provider".to_string(),
                toml::Value::String("mock".to_string()),
            ),
            (
                "model".to_string(),
                toml::Value::String("mock-merge".to_string()),
            ),
            ("max_tokens".to_string(), toml::Value::Integer(4096)),
        ]),
    );
    crate::llm_config::set_user_overrides(Some(overlay));
    super::super::reset_provider_key_cache();

    let mut options = model_role_options("merge");
    options.put_str("model", "mock-explicit");
    options.insert(crate::value::intern_key("max_tokens"), VmValue::Int(512));
    let opts = extract_with_options(options).expect("options");

    assert_eq!(opts.provider, "mock");
    assert_eq!(opts.model, "mock-explicit");
    assert_eq!(opts.max_tokens, 512);

    crate::llm_config::clear_user_overrides();
    clear_merge_role_env();
    super::super::reset_provider_key_cache();
}

#[test]
fn merge_model_role_has_env_overrides() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    clear_merge_role_env();
    std::env::set_var("HARN_LLM_MERGE_PROVIDER", "mock");
    std::env::set_var("HARN_LLM_MERGE_MODEL", "mock-env-merge");
    super::super::reset_provider_key_cache();

    let opts = extract_with_options(model_role_options("merge")).expect("options");

    assert_eq!(opts.provider, "mock");
    assert_eq!(opts.model, "mock-env-merge");

    clear_merge_role_env();
    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();
}

#[test]
fn model_role_aliases_do_not_override_exact_role_defaults() {
    let _guard = crate::llm::env_guard();
    clear_merge_role_env();
    let mut overlay = ProvidersConfig::default();
    overlay.model_roles.insert(
        "fast_apply".to_string(),
        std::collections::BTreeMap::from_iter([
            (
                "provider".to_string(),
                toml::Value::String("mock".to_string()),
            ),
            (
                "model".to_string(),
                toml::Value::String("mock-fast-apply".to_string()),
            ),
        ]),
    );
    overlay.model_roles.insert(
        "merge".to_string(),
        std::collections::BTreeMap::from_iter([
            (
                "provider".to_string(),
                toml::Value::String("mock".to_string()),
            ),
            (
                "model".to_string(),
                toml::Value::String("mock-merge".to_string()),
            ),
        ]),
    );
    crate::llm_config::set_user_overrides(Some(overlay));
    super::super::reset_provider_key_cache();

    let merge_opts = extract_with_options(model_role_options("merge")).expect("merge options");
    let fast_apply_opts =
        extract_with_options(model_role_options("fast_apply")).expect("fast_apply options");

    assert_eq!(merge_opts.model, "mock-merge");
    assert_eq!(fast_apply_opts.model, "mock-fast-apply");

    crate::llm_config::clear_user_overrides();
    clear_merge_role_env();
    super::super::reset_provider_key_cache();
}

#[test]
fn model_role_env_aliases_do_not_override_exact_role_env() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    clear_merge_role_env();
    std::env::set_var("HARN_LLM_FAST_APPLY_PROVIDER", "mock");
    std::env::set_var("HARN_LLM_FAST_APPLY_MODEL", "mock-env-fast-apply");
    std::env::set_var("HARN_LLM_MERGE_PROVIDER", "mock");
    std::env::set_var("HARN_LLM_MERGE_MODEL", "mock-env-merge");
    super::super::reset_provider_key_cache();

    let merge_opts = extract_with_options(model_role_options("merge")).expect("merge options");
    let fast_apply_opts =
        extract_with_options(model_role_options("fast_apply")).expect("fast_apply options");

    assert_eq!(merge_opts.model, "mock-env-merge");
    assert_eq!(fast_apply_opts.model, "mock-env-fast-apply");

    clear_merge_role_env();
    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();
}

#[test]
fn model_role_config_keys_normalize_like_call_options() {
    let _guard = crate::llm::env_guard();
    clear_merge_role_env();
    let mut overlay = ProvidersConfig::default();
    overlay.model_roles.insert(
        "fast-apply".to_string(),
        std::collections::BTreeMap::from_iter([
            (
                "provider".to_string(),
                toml::Value::String("mock".to_string()),
            ),
            (
                "model".to_string(),
                toml::Value::String("mock-fast-apply".to_string()),
            ),
        ]),
    );
    crate::llm_config::set_user_overrides(Some(overlay));
    super::super::reset_provider_key_cache();

    let opts = extract_with_options(model_role_options("fast_apply")).expect("options");

    assert_eq!(opts.provider, "mock");
    assert_eq!(opts.model, "mock-fast-apply");

    crate::llm_config::clear_user_overrides();
    clear_merge_role_env();
    super::super::reset_provider_key_cache();
}

fn fast_options(model: &str) -> crate::value::DictMap {
    let mut options = crate::value::DictMap::new();
    options.put_str("model", model);
    options.insert(crate::value::intern_key("fast"), VmValue::Bool(true));
    options
}

#[test]
fn fast_opts_into_tier_for_supported_model_and_guards_others() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();
    std::env::set_var("ANTHROPIC_API_KEY", "test-key");
    std::env::set_var("OPENAI_API_KEY", "test-key");
    super::super::reset_provider_key_cache();

    match extract_with_options(fast_options("claude-opus-4-8")) {
        Ok(opus) => assert!(opus.fast, "fast must be set for a model with a usable tier"),
        Err(e) => panic!("opus fast should succeed: {e:?}"),
    }

    // No fast tier -> rejected with a clear diagnostic.
    match extract_with_options(fast_options("gpt-4o")) {
        Err(VmError::Thrown(VmValue::String(message))) => {
            assert!(message.contains("no accelerated-serving tier"), "{message}");
        }
        other => panic!("expected thrown error for gpt-4o, got {:?}", other.is_ok()),
    }

    // Opus 4.6's fast tier has been removed from the catalog after
    // Anthropic's June 29, 2026 removal date.
    match extract_with_options(fast_options("claude-opus-4-6")) {
        Err(VmError::Thrown(VmValue::String(message))) => {
            assert!(message.contains("no accelerated-serving tier"), "{message}");
        }
        other => panic!(
            "expected thrown error for opus 4.6, got {:?}",
            other.is_ok()
        ),
    }

    // Deprecated tier -> rejected. Keep this covered with a synthetic catalog
    // row now that no built-in model has a deprecated fast tier.
    let overlay = crate::llm_config::parse_config_toml(
        r#"
[models."test-deprecated-fast"]
name = "Deprecated Fast Test"
provider = "anthropic"
context_window = 128000
fast_mode = { param = "speed", value = "fast", status = "deprecated", note = "removed" }
"#,
    )
    .expect("test catalog overlay");
    crate::llm_config::set_user_overrides(Some(overlay));
    super::super::reset_provider_key_cache();
    match extract_with_options(fast_options("test-deprecated-fast")) {
        Err(VmError::Thrown(VmValue::String(message))) => {
            assert!(message.contains("deprecated"), "{message}");
        }
        other => panic!(
            "expected thrown error for deprecated fast tier, got {:?}",
            other.is_ok()
        ),
    }

    std::env::remove_var("ANTHROPIC_API_KEY");
    std::env::remove_var("OPENAI_API_KEY");
    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();
}

#[test]
fn fastest_over_quality_selects_lowest_latency_available_candidate() {
    install_test_routes();
    let opts = extract_with_policy("fastest_over_quality(mid)");
    assert_eq!(opts.provider, "fast");
    assert_eq!(opts.model, "fast-mid-model");
    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();
}

#[test]
fn preference_list_cheapest_first_sets_route_fallbacks() {
    install_test_routes();
    let mut policy = crate::value::DictMap::new();
    policy.put_str("mode", "preference_list");
    policy.put_str("strategy", "cheapest_first");
    policy.insert(
        crate::value::intern_key("prefer"),
        VmValue::List(std::sync::Arc::new(vec![
            VmValue::String(arcstr::ArcStr::from("fast-mid")),
            VmValue::String(arcstr::ArcStr::from("cheap-mid")),
        ])),
    );
    let mut options = crate::value::DictMap::new();
    options.insert(
        crate::value::intern_key("route_policy"),
        VmValue::dict(policy),
    );
    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("options");

    assert_eq!(opts.provider, "cheap");
    assert_eq!(opts.model, "cheap-mid-model");
    assert_eq!(opts.route_fallbacks.len(), 1);
    assert_eq!(opts.route_fallbacks[0].provider, "fast");
    assert_eq!(opts.route_fallbacks[0].model, "fast-mid-model");
    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();
}

#[test]
fn equivalent_failover_builds_catalog_backed_routing_policy() {
    install_equivalent_routes();
    let mut failover = crate::value::DictMap::new();
    failover.insert(crate::value::intern_key("max_routes"), VmValue::Int(2));
    let opts = extract_with_options(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("primary")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("primary-model")),
        ),
        (
            crate::value::intern_key("equivalent_failover"),
            VmValue::dict(failover),
        ),
    ]))
    .expect("options");

    let policy = opts.routing_policy.expect("equivalent routing policy");
    assert_eq!(opts.provider, "primary");
    assert_eq!(opts.model, "primary-model");
    assert_eq!(policy.label, "equivalent_failover(primary:primary-model)");
    assert_eq!(policy.chain.len(), 2);
    assert_eq!(policy.chain[0].provider, "primary");
    assert_eq!(policy.chain[0].model, "primary-model");
    assert_eq!(policy.chain[1].provider, "backup-a");
    assert_eq!(policy.chain[1].model, "backup-a-model");
    assert!(policy
        .chain
        .iter()
        .all(|link| link.model != "same-provider-model"));
    assert_eq!(policy.failover.max_attempts, Some(2));
    assert!(!policy.failover.on_no_dispatch);

    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();
}

#[test]
fn equivalent_failover_enables_no_dispatch_failover_when_requested() {
    install_equivalent_routes();
    let mut failover = crate::value::DictMap::new();
    failover.insert(crate::value::intern_key("max_routes"), VmValue::Int(2));
    failover.insert(
        crate::value::intern_key("on_no_dispatch"),
        VmValue::Bool(true),
    );
    let opts = extract_with_options(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("primary")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("primary-model")),
        ),
        (
            crate::value::intern_key("equivalent_failover"),
            VmValue::dict(failover),
        ),
    ]))
    .expect("options");

    let policy = opts.routing_policy.expect("equivalent routing policy");
    assert_eq!(policy.failover.max_attempts, Some(2));
    assert!(policy.failover.on_no_dispatch);

    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();
}

#[test]
fn equivalent_failover_rejects_non_bool_no_dispatch_option() {
    install_equivalent_routes();
    let mut failover = crate::value::DictMap::new();
    failover.insert(
        crate::value::intern_key("on_no_dispatch"),
        VmValue::String(arcstr::ArcStr::from("yes")),
    );

    let err = match extract_with_options(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("primary")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("primary-model")),
        ),
        (
            crate::value::intern_key("equivalent_failover"),
            VmValue::dict(failover),
        ),
    ])) {
        Ok(_) => panic!("non-bool on_no_dispatch should fail"),
        Err(err) => err,
    };

    match err {
        VmError::Thrown(VmValue::String(message)) => {
            assert!(
                message.contains("equivalent_failover.on_no_dispatch"),
                "{message}"
            );
        }
        other => panic!("expected thrown on_no_dispatch error, got {other:?}"),
    }

    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();
}

#[test]
fn equivalent_failover_rejects_explicit_routing_policy() {
    install_equivalent_routes();
    let chain = VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
        std::sync::Arc::new(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("primary")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("primary-model")),
            ),
        ])),
    )]));
    let routing = crate::llm::routing::build_routing_policy(&crate::value::DictMap::from_iter([(
        crate::value::intern_key("chain"),
        chain,
    )]))
    .expect("routing policy");

    let err = match extract_with_options(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("primary")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("primary-model")),
        ),
        (crate::value::intern_key("routing"), routing),
        (
            crate::value::intern_key("equivalent_failover"),
            VmValue::Bool(true),
        ),
    ])) {
        Ok(_) => panic!("ambiguous routing should fail"),
        Err(err) => err,
    };

    match err {
        VmError::Thrown(VmValue::String(message)) => {
            assert!(message.contains("cannot be combined"), "{message}");
        }
        other => panic!("expected thrown ambiguity error, got {other:?}"),
    }

    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();
}

#[test]
fn always_policy_accepts_provider_model_selector() {
    install_test_routes();
    let opts = extract_with_policy("always(fast:fast-mid-model)");
    assert_eq!(opts.provider, "fast");
    assert_eq!(opts.model, "fast-mid-model");
    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();
}

#[test]
fn thinking_dict_enabled_false_disables_thinking() {
    let mut options = crate::value::DictMap::new();
    options.put_str("provider", "mock");
    options.put_str("model", "gpt-5.4");
    options.insert(
        crate::value::intern_key("thinking"),
        VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("enabled"),
            VmValue::Bool(false),
        )])),
    );
    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("options");
    assert!(opts.thinking.is_disabled());
}

#[test]
fn thinking_dict_enabled_budget_parses_typed_config() {
    let mut options = crate::value::DictMap::new();
    options.put_str("provider", "mock");
    options.put_str("model", "claude-opus-4-6");
    options.insert(
        crate::value::intern_key("thinking"),
        VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("mode"),
                VmValue::String(arcstr::ArcStr::from("enabled".to_string())),
            ),
            (
                crate::value::intern_key("budget_tokens"),
                VmValue::Int(8000),
            ),
        ])),
    );
    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("options");
    assert_eq!(
        opts.thinking,
        crate::llm::api::ThinkingConfig::Enabled {
            budget_tokens: Some(8000)
        }
    );
    assert_eq!(
        opts.anthropic_beta_features,
        vec![crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA]
    );
}

#[test]
fn anthropic_beta_features_parse_and_dedupe_with_interleaved_flag() {
    let mut options = crate::value::DictMap::new();
    options.put_str("provider", "mock");
    options.put_str("model", "claude-opus-4-6");
    options.insert(
        crate::value::intern_key("anthropic_beta_features"),
        VmValue::List(std::sync::Arc::new(vec![
            VmValue::String(arcstr::ArcStr::from(
                "fine-grained-tool-streaming-2025-05-14",
            )),
            VmValue::String(arcstr::ArcStr::from(
                crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA,
            )),
        ])),
    );
    options.insert(
        crate::value::intern_key("interleaved_thinking"),
        VmValue::Bool(true),
    );

    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("options");
    assert_eq!(
        opts.anthropic_beta_features,
        vec![
            "fine-grained-tool-streaming-2025-05-14".to_string(),
            crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA.to_string(),
        ]
    );
}

#[test]
fn anthropic_beta_features_reject_invalid_header_names() {
    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("claude-opus-4-6".to_string())),
        ),
        (
            crate::value::intern_key("anthropic_beta_features"),
            VmValue::String(arcstr::ArcStr::from("bad\r\nheader".to_string())),
        ),
    ]);

    let err = match extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ]) {
        Ok(_) => panic!("invalid beta feature should fail before transport"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("invalid beta feature name `bad"));
}

#[test]
fn thinking_effort_parses_typed_level() {
    let mut options = crate::value::DictMap::new();
    options.put_str("provider", "mock");
    options.put_str("model", "o3");
    options.insert(
        crate::value::intern_key("thinking"),
        VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("mode"),
                VmValue::String(arcstr::ArcStr::from("effort".to_string())),
            ),
            (
                crate::value::intern_key("level"),
                VmValue::String(arcstr::ArcStr::from("high".to_string())),
            ),
        ])),
    );
    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("options");
    assert_eq!(
        opts.thinking,
        crate::llm::api::ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::High
        }
    );
}

fn unsupported_local_options(extra: Vec<(&str, VmValue)>) -> VmError {
    let mut options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("local".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("unsupported-model".to_string())),
        ),
    ]);
    for (key, value) in extra {
        options.insert(crate::value::intern_key(key), value);
    }
    match extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ]) {
        Ok(_) => panic!("unsupported option should fail"),
        Err(err) => err,
    }
}

fn assert_unsupported_local_option(option: &str, extra: Vec<(&str, VmValue)>) {
    crate::llm::capabilities::clear_user_overrides();
    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();

    let err = unsupported_local_options(extra);

    assert!(
            err.to_string().contains(&format!(
                "option `{option}` is not supported by `unsupported-model` (provider `local`). See `harn provider catalog matrix` for compatibility."
            )),
            "unexpected error for {option}: {err}"
        );
}

fn one_tool_list() -> VmValue {
    VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
        std::sync::Arc::new(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("name"),
                VmValue::String(arcstr::ArcStr::from("lookup")),
            ),
            (
                crate::value::intern_key("description"),
                VmValue::String(arcstr::ArcStr::from("Look something up")),
            ),
            (
                crate::value::intern_key("parameters"),
                VmValue::dict(crate::value::DictMap::new()),
            ),
        ])),
    )]))
}

struct ScopedEnvVar {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedEnvVar {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        super::super::reset_provider_key_cache();
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
        super::super::reset_provider_key_cache();
    }
}

#[test]
fn unsupported_capability_options_error_with_provider_matrix_hint() {
    assert_unsupported_local_option("thinking", vec![("thinking", VmValue::Bool(true))]);
    assert_unsupported_local_option(
        "output_format",
        vec![(
            "output_format",
            VmValue::String(arcstr::ArcStr::from("json_object".to_string())),
        )],
    );
    assert_unsupported_local_option(
        "tools",
        vec![
            (
                "tool_format",
                VmValue::String(arcstr::ArcStr::from("native".to_string())),
            ),
            ("tools", one_tool_list()),
        ],
    );
    assert_unsupported_local_option("cache", vec![("cache", VmValue::Bool(true))]);
    assert_unsupported_local_option("vision", vec![("vision", VmValue::Bool(true))]);
    assert_unsupported_local_option("audio", vec![("audio", VmValue::Bool(true))]);
    assert_unsupported_local_option("pdf", vec![("pdf", VmValue::Bool(true))]);
    assert_unsupported_local_option("video", vec![("video", VmValue::Bool(true))]);
    assert_unsupported_local_option(
        "reasoning_effort",
        vec![(
            "reasoning_effort",
            VmValue::String(arcstr::ArcStr::from("high".to_string())),
        )],
    );
    assert_unsupported_local_option(
        "interleaved_thinking",
        vec![("interleaved_thinking", VmValue::Bool(true))],
    );
}

#[test]
fn tool_choice_accepted_on_text_tool_routes() {
    // devstral-small-2 on Ollama is native_tools=false but
    // text_tool_wire_format_supported=true. tool_choice should not
    // be rejected on text-format routes (e.g. agent scripts that
    // pass tool_choice="none" to suppress further tool calls).
    crate::llm::capabilities::clear_user_overrides();
    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();

    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("ollama".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("devstral-small-2:24b".to_string())),
        ),
        (
            crate::value::intern_key("tool_choice"),
            VmValue::String(arcstr::ArcStr::from("none".to_string())),
        ),
    ]);
    extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("tool_choice accepted on text-format routes");
}

#[test]
fn text_tool_format_does_not_emit_native_provider_tools() {
    crate::llm::capabilities::clear_user_overrides();
    crate::llm_config::clear_user_overrides();
    super::super::reset_provider_key_cache();

    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("ollama".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("devstral-small-2:24b".to_string())),
        ),
        (
            crate::value::intern_key("tool_format"),
            VmValue::String(arcstr::ArcStr::from("text".to_string())),
        ),
        (crate::value::intern_key("tools"), one_tool_list()),
    ]);
    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("text-format tools accepted");

    assert!(opts.tools.is_some());
    assert!(opts.native_tools.is_none());
}

#[test]
fn standalone_reasoning_effort_maps_to_thinking_effort_when_supported() {
    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("o3".to_string())),
        ),
        (
            crate::value::intern_key("reasoning_effort"),
            VmValue::String(arcstr::ArcStr::from("high".to_string())),
        ),
    ]);

    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("supported reasoning_effort");

    assert_eq!(
        opts.thinking,
        crate::llm::api::ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::High
        }
    );
}

#[test]
fn standalone_reasoning_effort_accepts_minimal_when_supported() {
    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("o3".to_string())),
        ),
        (
            crate::value::intern_key("reasoning_effort"),
            VmValue::String(arcstr::ArcStr::from("minimal".to_string())),
        ),
    ]);

    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("minimal reasoning_effort");

    assert_eq!(
        opts.thinking,
        crate::llm::api::ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::Minimal
        }
    );
}

#[test]
fn reasoning_policy_maps_to_provider_aware_thinking_when_explicit() {
    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("gpt-5.5".to_string())),
        ),
        (
            crate::value::intern_key("reasoning_policy"),
            VmValue::String(arcstr::ArcStr::from("off".to_string())),
        ),
    ]);

    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("reasoning_policy");

    assert_eq!(
        opts.thinking,
        crate::llm::api::ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::None
        }
    );
}

#[test]
fn session_pinned_reasoning_policy_is_llm_call_default() {
    crate::agent_sessions::reset_session_store();
    let session_id =
        crate::agent_sessions::open_or_create(Some("reasoning-policy-options-session".to_string()));
    crate::agent_sessions::set_pinned_reasoning_policy(&session_id, Some("high".to_string()))
        .expect("set policy");
    let _session_guard = crate::agent_sessions::enter_current_session(session_id);
    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("o3".to_string())),
        ),
    ]);

    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("session reasoning_policy");

    drop(_session_guard);
    crate::agent_sessions::reset_session_store();

    assert_eq!(
        opts.thinking,
        crate::llm::api::ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::High
        }
    );
}

#[test]
fn explicit_thinking_wins_over_session_pinned_reasoning_policy() {
    crate::agent_sessions::reset_session_store();
    let session_id = crate::agent_sessions::open_or_create(Some(
        "reasoning-policy-explicit-session".to_string(),
    ));
    crate::agent_sessions::set_pinned_reasoning_policy(&session_id, Some("high".to_string()))
        .expect("set policy");
    let _session_guard = crate::agent_sessions::enter_current_session(session_id);
    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("o3".to_string())),
        ),
        (crate::value::intern_key("thinking"), VmValue::Bool(false)),
    ]);

    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("explicit thinking");

    drop(_session_guard);
    crate::agent_sessions::reset_session_store();

    assert!(opts.thinking.is_disabled());
}

#[test]
fn standalone_reasoning_effort_accepts_none_and_xhigh_when_supported() {
    for (raw, expected) in [
        ("none", crate::llm::api::ReasoningEffort::None),
        ("xhigh", crate::llm::api::ReasoningEffort::XHigh),
    ] {
        let options = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("mock".to_string())),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("gpt-5.5".to_string())),
            ),
            (
                crate::value::intern_key("reasoning_effort"),
                VmValue::String(arcstr::ArcStr::from(raw.to_string())),
            ),
        ]);

        let opts = extract_llm_options(&[
            VmValue::String(arcstr::ArcStr::from("hello".to_string())),
            VmValue::Nil,
            VmValue::dict(options),
        ])
        .expect("reasoning_effort");

        assert_eq!(
            opts.thinking,
            crate::llm::api::ThinkingConfig::Effort { level: expected }
        );
    }
}

#[test]
fn standalone_reasoning_effort_rejects_cerebras_gpt_oss_unsupported_levels() {
    let _cerebras_key = ScopedEnvVar::set("CEREBRAS_API_KEY", "test-key");
    for (key, value) in [
        (
            "reasoning_effort",
            VmValue::String(arcstr::ArcStr::from("none".to_string())),
        ),
        (
            "thinking",
            VmValue::dict(crate::value::DictMap::from_iter([
                (
                    crate::value::intern_key("mode"),
                    VmValue::String(arcstr::ArcStr::from("effort".to_string())),
                ),
                (
                    crate::value::intern_key("level"),
                    VmValue::String(arcstr::ArcStr::from("minimal".to_string())),
                ),
            ])),
        ),
    ] {
        let options = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("cerebras".to_string())),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("gpt-oss-120b".to_string())),
            ),
            (crate::value::intern_key(key), value),
        ]);

        let err = match extract_with_options(options) {
            Ok(_) => panic!("unsupported reasoning effort should fail before transport"),
            Err(err) => err,
        };

        let message = err.to_string();
        assert!(
            message.contains("supported reasoning_effort values: low, medium, high"),
            "unexpected error: {message}"
        );
    }
}

#[test]
fn standalone_reasoning_effort_none_disables_thinking_without_effort_capability() {
    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("local".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("no-effort-model".to_string())),
        ),
        (
            crate::value::intern_key("reasoning_effort"),
            VmValue::String(arcstr::ArcStr::from("none".to_string())),
        ),
    ]);

    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("reasoning_effort none should be universally accepted");

    assert_eq!(
        opts.thinking,
        crate::llm::api::ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::None
        }
    );
}

#[test]
fn standalone_reasoning_effort_uses_dedicated_capability_gate() {
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.local]]
model_match = "thinking-effort-only"
thinking_modes = ["effort"]
"#,
    )
    .expect("capability override");
    super::super::reset_provider_key_cache();

    let err = unsupported_local_options(vec![
        (
            "model",
            VmValue::String(arcstr::ArcStr::from("thinking-effort-only".to_string())),
        ),
        (
            "reasoning_effort",
            VmValue::String(arcstr::ArcStr::from("high".to_string())),
        ),
    ]);

    assert!(
        err.to_string()
            .contains("option `reasoning_effort` is not supported"),
        "unexpected error: {err}"
    );
    crate::llm::capabilities::clear_user_overrides();
}

#[test]
fn image_content_sets_vision_and_requires_capability() {
    let image_block = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("type"),
            VmValue::String(arcstr::ArcStr::from("image")),
        ),
        (
            crate::value::intern_key("base64"),
            VmValue::String(arcstr::ArcStr::from("iVBORw0KGgo=")),
        ),
        (
            crate::value::intern_key("media_type"),
            VmValue::String(arcstr::ArcStr::from("image/png")),
        ),
    ]));
    let message = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("role"),
            VmValue::String(arcstr::ArcStr::from("user")),
        ),
        (
            crate::value::intern_key("content"),
            VmValue::List(std::sync::Arc::new(vec![image_block])),
        ),
    ]));
    let options = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("gpt-4o")),
        ),
        (
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(vec![message.clone()])),
        ),
    ]));
    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("")),
        VmValue::Nil,
        options,
    ])
    .unwrap();
    assert!(opts.vision);

    let bad_options = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("gpt-3.5-turbo")),
        ),
        (
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(vec![message])),
        ),
    ]));
    let err = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("")),
        VmValue::Nil,
        bad_options,
    ])
    .err()
    .expect("non-vision model should reject image content");
    assert!(err.to_string().contains("option `vision` is not supported"));

    let url_image = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("type"),
            VmValue::String(arcstr::ArcStr::from("image")),
        ),
        (
            crate::value::intern_key("url"),
            VmValue::String(arcstr::ArcStr::from("https://example.com/image.png")),
        ),
        (
            crate::value::intern_key("media_type"),
            VmValue::String(arcstr::ArcStr::from("image/png")),
        ),
    ]));
    let url_message = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("role"),
            VmValue::String(arcstr::ArcStr::from("user")),
        ),
        (
            crate::value::intern_key("content"),
            VmValue::List(std::sync::Arc::new(vec![url_image])),
        ),
    ]));
    let ollama_options = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("ollama")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("llava:latest")),
        ),
        (
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(vec![url_message])),
        ),
    ]));
    let err = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("")),
        VmValue::Nil,
        ollama_options,
    ])
    .err()
    .expect("ollama should reject url image content");
    assert!(err.to_string().contains("requires image base64"));
}

#[test]
fn pdf_and_audio_content_require_capabilities() {
    let pdf_block = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("type"),
            VmValue::String(arcstr::ArcStr::from("pdf")),
        ),
        (
            crate::value::intern_key("file_id"),
            VmValue::String(arcstr::ArcStr::from("file_123")),
        ),
    ]));
    let audio_block = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("type"),
            VmValue::String(arcstr::ArcStr::from("audio")),
        ),
        (
            crate::value::intern_key("base64"),
            VmValue::String(arcstr::ArcStr::from("UklGRg==")),
        ),
        (
            crate::value::intern_key("media_type"),
            VmValue::String(arcstr::ArcStr::from("audio/wav")),
        ),
    ]));
    let message = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("role"),
            VmValue::String(arcstr::ArcStr::from("user")),
        ),
        (
            crate::value::intern_key("content"),
            VmValue::List(std::sync::Arc::new(vec![pdf_block, audio_block])),
        ),
    ]));
    let options = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("claude-sonnet-4-7")),
        ),
        (
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(vec![message.clone()])),
        ),
    ]));
    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("")),
        VmValue::Nil,
        options,
    ])
    .unwrap();
    assert!(opts
        .anthropic_beta_features
        .contains(&crate::stdlib::files::ANTHROPIC_FILES_API_BETA.to_string()));

    let bad_options = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("gpt-3.5-turbo")),
        ),
        (
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(vec![message])),
        ),
    ]));
    let err = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("")),
        VmValue::Nil,
        bad_options,
    ])
    .err()
    .expect("non-multimodal model should reject pdf/audio content");
    assert!(err.to_string().contains("option `audio` is not supported"));
}

#[test]
fn video_content_requires_capability() {
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.local]]
model_match = "video-model"
video_supported = true
"#,
    )
    .expect("capability override");
    let video_block = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("type"),
            VmValue::String(arcstr::ArcStr::from("video")),
        ),
        (
            crate::value::intern_key("base64"),
            VmValue::String(arcstr::ArcStr::from("AAAA")),
        ),
        (
            crate::value::intern_key("media_type"),
            VmValue::String(arcstr::ArcStr::from("video/mp4")),
        ),
    ]));
    let message = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("role"),
            VmValue::String(arcstr::ArcStr::from("user")),
        ),
        (
            crate::value::intern_key("content"),
            VmValue::List(std::sync::Arc::new(vec![video_block])),
        ),
    ]));
    let options = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("local")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("video-model")),
        ),
        (
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(vec![message.clone()])),
        ),
    ]));
    extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("")),
        VmValue::Nil,
        options,
    ])
    .expect("video-capable route should accept video content");
    crate::llm::capabilities::clear_user_overrides();

    let bad_options = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("gpt-4o")),
        ),
        (
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(vec![message])),
        ),
    ]));
    let err = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("")),
        VmValue::Nil,
        bad_options,
    ])
    .err()
    .expect("non-video model should reject video content");
    assert!(err.to_string().contains("option `video` is not supported"));
}
