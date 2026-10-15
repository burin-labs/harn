//! Shared fixtures for the `lookup_tests_*` modules.
//!
//! These helpers are used by more than one themed module, so they live here
//! rather than being duplicated or reached for across sibling test modules.

use super::{
    admit_portable_option_for_thinking, clear_user_overrides, lookup, PortableOption,
    ReasoningEffort,
};

/// Drop any user overrides a previous test installed.
///
/// The capability registry is process-global, so a test that sets overrides
/// and does not clear them leaks into whatever runs next.
pub(super) fn reset() {
    clear_user_overrides();
}

/// Assert a Cerebras route exposes effort-style reasoning with the expected
/// thinking-block style.
///
/// `tool_format` is deliberately NOT asserted here: Cerebras gpt-oss and
/// zai-glm have different defaults (gpt-oss harmonized to `json`, glm stays
/// `native`), and this helper is about reasoning-effort behavior. Tool-format
/// resolution is asserted in the dedicated harmonization tests.
pub(super) fn assert_cerebras_effort_reasoning(model: &str, thinking_block_style: &str) {
    let caps = lookup("cerebras", model);
    assert_eq!(caps.thinking_modes, vec!["effort"]);
    assert!(caps.reasoning_effort_supported);
    assert_eq!(caps.structured_output.as_deref(), Some("native"));
    assert_eq!(caps.structured_output_mode, "native_json");
    assert_eq!(caps.thinking_block_style, thinking_block_style);
}

#[test]
fn override_can_supply_anthropic_beta_features() {
    reset();
    super::set_user_overrides_toml(
        r#"
[[provider.anthropic]]
model_match = "claude-custom-*"
native_tools = true
anthropic_beta_features = ["fine-grained-tool-streaming-2025-05-14"]
"#,
    )
    .unwrap();
    let caps = lookup("anthropic", "claude-custom-1");
    assert_eq!(
        caps.anthropic_beta_features,
        ["fine-grained-tool-streaming-2025-05-14"]
    );
    assert!(lookup("anthropic", "unmatched-custom-model")
        .anthropic_beta_features
        .is_empty());
    reset();
}

#[test]
fn gpt6_sampling_condition_is_direct_and_not_inherited_by_unforwarded_routes() {
    reset();
    let none = crate::llm::api::ThinkingConfig::Effort {
        level: ReasoningEffort::None,
    };
    let low = crate::llm::api::ThinkingConfig::Effort {
        level: ReasoningEffort::Low,
    };
    for (provider, model) in [("openai", "gpt-6-luna"), ("openai", "gpt-6-sol")] {
        for thinking in [&crate::llm::api::ThinkingConfig::Disabled, &low] {
            assert!(
                admit_portable_option_for_thinking(
                    provider,
                    model,
                    thinking,
                    PortableOption::Temperature,
                    None
                )
                .is_err(),
                "{provider}:{model}"
            );
        }
        admit_portable_option_for_thinking(
            provider,
            model,
            &none,
            PortableOption::Temperature,
            None,
        )
        .expect("explicit none admits temperature");
    }
    for model in ["openai/gpt-6-luna", "openai/gpt-6-sol"] {
        admit_portable_option_for_thinking(
            "openrouter",
            model,
            &none,
            PortableOption::Temperature,
            None,
        )
        .expect_err("an unforwarded option must not inherit direct conditional support");
    }
    admit_portable_option_for_thinking(
        "openai",
        "gpt-6-astra",
        &none,
        PortableOption::Temperature,
        None,
    )
    .expect_err("Astra must not inherit a condition for Sol and Luna");
}

#[test]
fn mistral_large_4_direct_route_sends_the_card_api_name() {
    assert_eq!(
        crate::llm_config::wire_model_id("mistral-large-4-0"),
        "mistral-large-4"
    );
    assert_eq!(
        crate::llm_config::wire_model_id("mistralai/mistral-large-4-0"),
        "mistralai/mistral-large-4-0"
    );
    assert_eq!(
        crate::llm_config::wire_model_id("mistral-small-2603"),
        "mistral-small-2603"
    );
}
