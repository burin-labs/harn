//! Selector resolution and per-call option merging.

use super::super::selection_builtins::{
    llm_model_defaults_builtin, llm_reasoning_effort_budget_builtin, llm_resolved_options_builtin,
};
use super::fixtures::build_dict;
use crate::llm_config;
use crate::value::{VmError, VmValue};

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
fn test_llm_reasoning_effort_budget_matches_canonical_mapping() {
    let mut out = String::new();
    for level in [
        "minimal", "low", "medium", "high", "xhigh", "max", "", "unknown",
    ] {
        let args = vec![VmValue::String(arcstr::ArcStr::from(level))];
        let result =
            llm_reasoning_effort_budget_builtin(&args, &mut out).expect("builtin returned error");
        let got = match result {
            VmValue::Int(n) => n,
            other => panic!("expected Int, got {other:?}"),
        };
        let expected = i64::from(crate::llm::reasoning_policy::budget_for_reasoning_level(
            level,
        ));
        assert_eq!(got, expected, "budget mismatch for level {level:?}");
    }
}

#[test]
fn test_llm_resolved_options_requires_model() {
    llm_config::clear_user_overrides();
    let mut out = String::new();
    let args = vec![build_dict(vec![])];
    let err =
        llm_resolved_options_builtin(&args, &mut out).expect_err("missing model should error");
    match err {
        VmError::Runtime(message) => {
            assert!(
                message.contains("opts.model is required"),
                "unexpected message: {message}"
            );
        }
        other => panic!("expected Runtime error, got {other:?}"),
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
