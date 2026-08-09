use super::*;
use crate::llm::api::{ReasoningEffort, ThinkingConfig};
use crate::llm_config::{ModelDef, ProvidersConfig};
use crate::value::VmDictExt;

fn logical_test_model() -> ModelDef {
    let mut model =
        crate::llm_config::model_catalog_entry("gpt-5.4").expect("built-in GPT-5.4 catalog row");
    model.name = "Logical default test model".to_string();
    model.provider = "mock".to_string();
    model.logical_model = Some("logical-default-test".to_string());
    model
}

fn extract(options: crate::value::DictMap) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello")),
        VmValue::Nil,
        VmValue::dict(options),
    ])
}

#[test]
fn logical_defaults_flow_through_request_options_with_caller_precedence() {
    let _guard = crate::llm::env_guard();
    let mut overlay = ProvidersConfig::default();
    let model_id = "gpt-5-logical-default-test";
    overlay
        .models
        .insert(model_id.to_string(), logical_test_model());
    overlay.model_defaults.insert(
        "logical:logical-default-test".to_string(),
        std::collections::BTreeMap::from_iter([
            ("temperature".to_string(), toml::Value::Float(1.0)),
            ("top_p".to_string(), toml::Value::Float(0.95)),
            (
                "reasoning_effort".to_string(),
                toml::Value::String("high".to_string()),
            ),
        ]),
    );
    crate::llm_config::set_user_overrides(Some(overlay));

    let mut options = crate::value::DictMap::new();
    options.put_str("provider", "mock");
    options.put_str("model", model_id);
    let defaults = extract(options.clone()).expect("logical defaults");
    assert_eq!(defaults.temperature, Some(1.0));
    assert_eq!(defaults.top_p, Some(0.95));
    assert_eq!(
        defaults.thinking,
        ThinkingConfig::Effort {
            level: ReasoningEffort::High
        }
    );

    options.insert(crate::value::intern_key("temperature"), VmValue::Float(0.2));
    options.put_str("effort", "low");
    let explicit = extract(options.clone()).expect("caller overrides");
    assert_eq!(explicit.temperature, Some(0.2));
    assert_eq!(explicit.top_p, Some(0.95));
    assert_eq!(
        explicit.thinking,
        ThinkingConfig::Effort {
            level: ReasoningEffort::Low
        }
    );

    options.remove("effort");
    options.insert(crate::value::intern_key("thinking"), VmValue::Bool(false));
    let disabled = extract(options).expect("explicit disabled thinking");
    assert_eq!(disabled.thinking, ThinkingConfig::Disabled);

    crate::llm_config::clear_user_overrides();
}

#[test]
fn fixed_server_value_default_is_not_mistaken_for_caller_intent() {
    let _guard = crate::llm::env_guard();
    let model_id = "fixed-server-value-test";
    let mut overlay = ProvidersConfig::default();
    overlay.model_defaults.insert(
        format!("local/{model_id}"),
        std::collections::BTreeMap::from_iter([(
            "temperature".to_string(),
            toml::Value::Float(1.0),
        )]),
    );
    crate::llm_config::set_user_overrides(Some(overlay));
    crate::llm::capabilities::set_user_overrides_toml(&format!(
        r#"
[[provider.local]]
model_match = "{model_id}"
temperature_supported = false
"#
    ))
    .expect("capability override");

    let base = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("local")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from(model_id)),
        ),
    ]);
    let defaults = extract(base.clone()).expect("catalog default should remain admissible");
    assert_eq!(defaults.temperature, Some(1.0));
    assert!(!defaults
        .portable_option_intent
        .contains(&crate::llm::capabilities::PortableOption::Temperature));

    let mut explicit = base;
    explicit.insert(crate::value::intern_key("temperature"), VmValue::Float(1.0));
    let error = extract(explicit).expect_err("caller intent must be admitted");
    assert!(
        error
            .to_string()
            .contains("option `temperature` is not supported"),
        "unexpected error: {error}"
    );

    crate::llm::capabilities::clear_user_overrides();
    crate::llm_config::clear_user_overrides();
}
