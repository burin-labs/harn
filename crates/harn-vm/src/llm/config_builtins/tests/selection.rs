//! Selector resolution and per-call option merging.

use super::super::selection_builtins::{
    llm_escalation_target_builtin, llm_model_defaults_builtin, llm_model_ladder_builtin,
    llm_resolved_options_builtin, parse_complementary_reviewer_options,
    parse_escalation_target_options,
};
use super::fixtures::build_dict;
use crate::llm_config;
use crate::value::VmValue;

#[test]
fn complementary_reviewer_budget_options_cross_builtin_boundary() {
    let options = build_dict(vec![
        ("author_model", VmValue::string("gpt-6-luna")),
        ("max_price_multiplier", VmValue::Float(3.0)),
        ("min_price_cap_per_mtok", VmValue::Float(6.0)),
        ("max_price_cap_per_mtok", VmValue::Float(15.0)),
    ]);
    let parsed = parse_complementary_reviewer_options(Some(&options)).expect("valid budget");
    assert_eq!(parsed.max_price_multiplier, Some(3.0));
    assert_eq!(parsed.min_price_cap_per_mtok, Some(6.0));
    assert_eq!(parsed.max_price_cap_per_mtok, Some(15.0));

    let inverted = build_dict(vec![
        ("author_model", VmValue::string("gpt-6-luna")),
        ("min_price_cap_per_mtok", VmValue::Float(16.0)),
        ("max_price_cap_per_mtok", VmValue::Float(15.0)),
    ]);
    assert!(parse_complementary_reviewer_options(Some(&inverted))
        .expect_err("inverted budget must fail")
        .to_string()
        .contains("min_price_cap_per_mtok must not exceed"),);
}

fn primary(model: &str) -> VmValue {
    build_dict(vec![("model", VmValue::string(model))])
}

#[test]
fn escalation_target_options_cross_builtin_boundary() {
    let parsed = parse_escalation_target_options(Some(&build_dict(vec![
        ("primary", primary(" small ")),
        ("max_price_multiple", VmValue::Int(3)),
        ("exclude", VmValue::list(vec![VmValue::string("fam-b")])),
    ])))
    .expect("valid options");
    assert_eq!(parsed.primary_model, "small");
    assert!(parsed.available_only, "credentials are required by default");
    assert_eq!(parsed.max_price_multiple, Some(3.0));
    assert_eq!(parsed.exclude, vec!["fam-b".to_string()]);

    for (bad, needle) in [
        (build_dict(vec![]), "primary.model is required"),
        (
            build_dict(vec![
                ("primary", primary("m")),
                ("available_only", VmValue::string("yes")),
            ]),
            "available_only must be a bool",
        ),
        (
            build_dict(vec![
                ("primary", primary("m")),
                ("max_price_multiple", VmValue::Int(0)),
            ]),
            "max_price_multiple must be positive",
        ),
        (
            build_dict(vec![
                ("primary", primary("m")),
                ("exclude", VmValue::string("x")),
            ]),
            "exclude must be a list",
        ),
    ] {
        let error = parse_escalation_target_options(Some(&bad)).expect_err("rejected");
        assert!(error.to_string().contains(needle), "{needle}: {error}");
    }
}

/// The acceptance path: a selected target is a `models:` ladder step as
/// returned, with no adapter between the selector and the router.
#[test]
fn escalation_target_result_is_a_ladder_step_without_an_adapter() {
    llm_config::clear_user_overrides();
    let mut out = String::new();
    let result = llm_escalation_target_builtin(
        &[build_dict(vec![
            ("primary", primary("gpt-4o-mini")),
            ("available_only", VmValue::Bool(false)),
        ])],
        &mut out,
    )
    .expect("builtin never throws on a catalog miss");
    let step = result.as_dict().expect("dict");
    assert!(
        step.get("fallback_code").is_none(),
        "the real catalog has a stronger model than gpt-4o-mini: {result:?}"
    );

    let mut call = crate::value::DictMap::new();
    call.insert(
        crate::value::intern_key("models"),
        VmValue::list(vec![VmValue::string("gpt-4o-mini"), result.clone()]),
    );
    let policy = crate::llm::routing::build_model_ladder_policy(&call, "openai", "gpt-4o-mini")
        .expect("ladder accepts the selector result")
        .expect("a ladder policy");
    assert!(policy.is_ladder);
    assert_eq!(
        policy.chain.last().map(|link| link.model.clone()),
        step.get("model").map(|model| model.display()),
    );
}

#[test]
fn escalation_target_unknown_primary_returns_a_code_not_an_error() {
    let mut out = String::new();
    let result = llm_escalation_target_builtin(
        &[build_dict(vec![(
            "primary",
            primary("definitely-not-a-real-model-zzzzz"),
        )])],
        &mut out,
    )
    .expect("a catalog miss is a result");
    let dict = result.as_dict().expect("dict");
    assert_eq!(
        dict.get("fallback_code").map(|code| code.display()),
        Some("unknown_primary".to_string())
    );
    assert!(dict.get("model").is_none());
}

#[test]
fn test_llm_model_defaults_returns_empty_for_unknown_model() {
    llm_config::clear_user_overrides();
    let mut out = String::new();
    let args = vec![VmValue::string("definitely-not-a-real-model-id-zzzzz")];
    let result = llm_model_defaults_builtin(&args, &mut out).expect("builtin returned error");
    let dict = result.as_dict().expect("expected dict");
    assert!(
        dict.is_empty(),
        "unknown model should yield empty defaults dict, got {dict:?}"
    );
}

#[test]
fn test_llm_model_ladder_projects_catalog_owned_steps() {
    llm_config::clear_user_overrides();
    let mut out = String::new();
    let args = vec![VmValue::string("agent_frontier")];
    let result = llm_model_ladder_builtin(&args, &mut out).expect("catalog ladder");
    let ladder = result.as_dict().expect("ladder dict");
    let steps = match ladder.get("steps") {
        Some(VmValue::List(steps)) => steps,
        other => panic!("expected ladder steps, got {other:?}"),
    };
    assert_eq!(steps.len(), 2);
    assert_eq!(
        steps[0]
            .as_dict()
            .and_then(|step| step.get("provider"))
            .map(VmValue::display)
            .as_deref(),
        Some("anthropic")
    );
}

#[test]
fn test_llm_resolved_options_uses_dispatch_defaults_without_model() {
    let _guard = crate::llm::env_guard();
    llm_config::clear_user_overrides();
    let expected_provider = crate::llm::helpers::vm_resolve_provider(&None);
    let expected_model = crate::llm::helpers::vm_resolve_model(&None, &expected_provider).unwrap();
    let mut out = String::new();
    for options in [
        build_dict(vec![]),
        build_dict(vec![("model", VmValue::string(""))]),
    ] {
        let result = llm_resolved_options_builtin(&[options], &mut out).expect("default route");
        let fields = result.as_dict().expect("resolved options");
        assert_eq!(
            fields.get("provider").map(VmValue::display),
            Some(expected_provider.clone())
        );
        assert_eq!(
            fields.get("model").map(VmValue::display),
            Some(expected_model.clone())
        );
    }
}

#[test]
fn test_provider_without_default_requires_explicit_model() {
    let _guard = crate::llm::env_guard();
    let _env = crate::test_env::test_env_guard();
    llm_config::clear_user_overrides();
    let provider = "fixture-no-default";
    let model_id = "fixture-model";
    let mut overlay = llm_config::ProvidersConfig::default();
    overlay.providers.insert(
        provider.to_string(),
        llm_config::ProviderDef {
            base_url: "https://fixture.invalid/v1".to_string(),
            chat_endpoint: "/chat/completions".to_string(),
            ..Default::default()
        },
    );
    let mut model = llm_config::model_catalog_entries()
        .into_iter()
        .find(|(_, model)| {
            !model.deprecated
                && model.supports_operation(llm_config::ModelOperation::TextGeneration)
        })
        .expect("catalog has a text-generation model")
        .1;
    model.provider = provider.to_string();
    overlay.models.insert(model_id.to_string(), model);
    llm_config::set_user_overrides(Some(overlay));

    let mut out = String::new();
    let error = llm_resolved_options_builtin(
        &[build_dict(vec![("provider", VmValue::string(provider))])],
        &mut out,
    )
    .expect_err("provider without a default must fail before dispatch");
    let crate::value::VmError::Thrown(value) = error else {
        panic!("expected a typed model-resolution error");
    };
    let fields = value.as_dict().expect("typed error fields");
    assert_eq!(
        fields.get("code").map(VmValue::display).as_deref(),
        Some("model_resolution_failed")
    );
    assert!(fields
        .get("message")
        .is_some_and(|message| { message.display().contains("pass an explicit model") }));

    let result = llm_resolved_options_builtin(
        &[build_dict(vec![
            ("provider", VmValue::string(provider)),
            ("model", VmValue::string(model_id)),
        ])],
        &mut out,
    )
    .expect("explicit catalog model still resolves");
    let fields = result.as_dict().expect("resolved options");
    assert_eq!(
        fields.get("provider").map(VmValue::display).as_deref(),
        Some(provider)
    );
    assert_eq!(
        fields.get("model").map(VmValue::display).as_deref(),
        Some(model_id)
    );
    llm_config::clear_user_overrides();
}

#[test]
fn test_llm_resolved_options_user_wins_over_defaults() {
    let _guard = crate::llm::env_guard();
    llm_config::clear_user_overrides();
    let mut overlay = llm_config::ProvidersConfig::default();
    let mut model_defaults = std::collections::BTreeMap::new();
    model_defaults.insert(
        "fake-resolved-options-model".to_string(),
        toml::Value::Float(0.5),
    );
    overlay
        .model_defaults
        .insert("fake-resolved-options-model".to_string(), model_defaults);
    llm_config::set_user_overrides(Some(overlay));

    let mut out = String::new();
    let args = vec![build_dict(vec![
        ("model", VmValue::string("fake-resolved-options-model")),
        ("temperature", VmValue::Float(0.9)),
    ])];
    let result = llm_resolved_options_builtin(&args, &mut out).expect("builtin returned error");
    let dict = result.as_dict().expect("expected dict");
    match dict.get("temperature") {
        Some(VmValue::Float(f)) => assert!((*f - 0.9).abs() < 1e-9, "user value lost: {f}"),
        other => panic!("expected Float(0.9), got {other:?}"),
    }
    match dict.get("model") {
        Some(VmValue::String(s)) => assert_eq!(s.as_str(), "fake-resolved-options-model"),
        other => panic!("expected model string, got {other:?}"),
    }

    llm_config::clear_user_overrides();
}

#[test]
fn test_llm_resolved_options_default_fills_unspecified() {
    let _guard = crate::llm::env_guard();
    llm_config::clear_user_overrides();
    let mut overlay = llm_config::ProvidersConfig::default();
    let mut model_defaults = std::collections::BTreeMap::new();
    model_defaults.insert("temperature".to_string(), toml::Value::Float(0.5));
    overlay
        .model_defaults
        .insert("fake-fill-defaults-model".to_string(), model_defaults);
    llm_config::set_user_overrides(Some(overlay));

    let mut out = String::new();
    let args = vec![build_dict(vec![(
        "model",
        VmValue::string("fake-fill-defaults-model"),
    )])];
    let result = llm_resolved_options_builtin(&args, &mut out).expect("builtin returned error");
    let dict = result.as_dict().expect("expected dict");
    match dict.get("temperature") {
        Some(VmValue::Float(f)) => assert!((*f - 0.5).abs() < 1e-9, "default lost: {f}"),
        other => panic!("expected Float(0.5), got {other:?}"),
    }

    llm_config::clear_user_overrides();
}

#[test]
fn test_llm_resolved_options_resolves_provider() {
    let _guard = crate::llm::env_guard();
    let _env = crate::test_env::test_env_guard();
    llm_config::clear_user_overrides();

    let mut out = String::new();
    let args = vec![build_dict(vec![(
        "model",
        VmValue::string("claude-sonnet-4-20250514"),
    )])];
    let result = llm_resolved_options_builtin(&args, &mut out).expect("builtin returned error");
    let dict = result.as_dict().expect("expected dict");
    match dict.get("provider") {
        Some(VmValue::String(s)) => {
            assert_eq!(s.as_str(), "anthropic", "provider mismatch: {s}");
        }
        other => panic!("expected provider string, got {other:?}"),
    }
}
