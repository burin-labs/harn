use std::collections::BTreeMap;

use super::*;

#[test]
fn resolve_logical_defaults_across_every_gpt_oss_route() {
    let config = default_config();
    for (provider, model) in [
        ("cerebras", "gpt-oss-120b"),
        ("groq", "groq/openai/gpt-oss-120b"),
        ("fireworks", "accounts/fireworks/models/gpt-oss-120b"),
        ("openrouter", "openai/gpt-oss-120b"),
        ("nvidia", "nvidia/openai/gpt-oss-120b"),
        ("deepinfra", "deepinfra/openai/gpt-oss-120b"),
        ("sambanova", "sambanova/gpt-oss-120b"),
        ("baseten", "baseten/openai/gpt-oss-120b"),
    ] {
        let params = model_params_for_route_with_config(&config, provider, model);
        assert_eq!(
            params.get("temperature").and_then(toml::Value::as_float),
            Some(1.0),
            "{provider}:{model} temperature"
        );
        let expected_top_p = (provider != "baseten").then_some(1.0);
        assert_eq!(
            params.get("top_p").and_then(toml::Value::as_float),
            expected_top_p,
            "{provider}:{model} top_p"
        );
        assert_eq!(
            params.get("reasoning_effort").and_then(toml::Value::as_str),
            Some("high"),
            "{provider}:{model} reasoning effort"
        );
    }
    assert_eq!(model_default_issues(&config), Vec::<String>::new());
}

#[test]
fn route_default_overrides_logical_model_default() {
    let mut config = default_config();
    config.model_defaults.insert(
        "fireworks/accounts/fireworks/models/gpt-oss-120b".to_string(),
        BTreeMap::from_iter([("temperature".to_string(), toml::Value::Float(0.4))]),
    );

    let params = model_params_for_route_with_config(
        &config,
        "fireworks",
        "accounts/fireworks/models/gpt-oss-120b",
    );
    assert_eq!(
        params.get("temperature").and_then(toml::Value::as_float),
        Some(0.4)
    );
    assert_eq!(
        params.get("top_p").and_then(toml::Value::as_float),
        Some(1.0)
    );
    assert_eq!(
        params.get("reasoning_effort").and_then(toml::Value::as_str),
        Some("high")
    );

    config.model_defaults.insert(
        "groq/openai/gpt-oss-120b".to_string(),
        BTreeMap::from_iter([("temperature".to_string(), toml::Value::Float(0.2))]),
    );
    let wire_params = model_params_for_route_with_config(&config, "groq", "openai/gpt-oss-120b");
    assert_eq!(
        wire_params
            .get("temperature")
            .and_then(toml::Value::as_float),
        Some(0.2),
        "canonical route override must apply when the transport passes a wire id"
    );
}

#[test]
fn audit_rejects_unknown_and_malformed_logical_defaults() {
    let mut unknown = default_config();
    unknown.model_defaults.insert(
        "logical:not-a-catalog-model".to_string(),
        BTreeMap::from_iter([("temperature".to_string(), toml::Value::Float(1.0))]),
    );
    assert!(model_default_issues(&unknown).contains(
        &"model_defaults.logical:not-a-catalog-model references an unknown logical model"
            .to_string()
    ));

    let mut malformed = default_config();
    malformed.model_defaults.insert(
        "logical:openai-gpt-oss-120b".to_string(),
        BTreeMap::from_iter([
            ("temperature".to_string(), toml::Value::Float(3.0)),
            (
                "reasoning_effort".to_string(),
                toml::Value::String("extreme".to_string()),
            ),
        ]),
    );
    let issues = model_default_issues(&malformed);
    assert!(issues.contains(
        &"model_defaults.logical:openai-gpt-oss-120b.temperature is not a supported generation default"
            .to_string()
    ));
    assert!(issues.contains(
        &"model_defaults.logical:openai-gpt-oss-120b.reasoning_effort is not a supported generation default"
            .to_string()
    ));
}
