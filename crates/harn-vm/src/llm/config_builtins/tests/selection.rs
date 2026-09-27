//! Selector resolution and per-call option merging.

use super::super::selection_builtins::{
    llm_model_defaults_builtin, llm_model_ladder_builtin, llm_resolved_options_builtin,
    parse_complementary_reviewer_options,
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

#[test]
fn test_llm_model_defaults_returns_empty_for_unknown_model() {
    llm_config::clear_user_overrides();
    let mut out = String::new();
    let args = vec![VmValue::String(arcstr::ArcStr::from(
        "definitely-not-a-real-model-id-zzzzz",
    ))];
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
    let args = vec![VmValue::String(arcstr::ArcStr::from("agent_frontier"))];
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
    let expected_model = crate::llm::helpers::vm_resolve_model(&None, &expected_provider);
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
        (
            "model",
            VmValue::String(arcstr::ArcStr::from("fake-resolved-options-model")),
        ),
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
        VmValue::String(arcstr::ArcStr::from("fake-fill-defaults-model")),
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
        VmValue::String(arcstr::ArcStr::from("claude-sonnet-4-20250514")),
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
