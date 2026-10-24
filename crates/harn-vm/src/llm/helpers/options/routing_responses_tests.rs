use super::routing_test_support::{
    extract_with_options, one_tool_list, test_equivalent_model, test_provider, ScopedEnvVar,
};
use super::*;
use crate::llm::helpers::options::routing::equivalent_failover_requirements_for_options;
use crate::llm_config::ProvidersConfig;

#[test]
fn gpt_5_4_mini_reasoning_tools_preserve_route_constraints() {
    use crate::llm::api::{effective_tool_api_mode, LlmApiMode, ReasoningEffort, ThinkingConfig};

    crate::llm::capabilities::clear_user_overrides();
    let thinking = ThinkingConfig::Effort {
        level: ReasoningEffort::Low,
    };
    for (provider, model, requires_responses) in [
        ("openai", "gpt-5.4-mini", true),
        ("openai", "gpt-5.4-mini-2026-03-17", true),
        ("openai", "gpt-5.4-mini-2099-01-01", false),
        ("openai", "gpt-5.4", false),
        ("openai", "gpt-5.4-nano", false),
        ("openai", "gpt-5.1", false),
        ("openrouter", "openai/gpt-5.4-mini", false),
        ("vercel_ai_gateway", "openai/gpt-5.4-mini", false),
    ] {
        let caps = crate::llm::capabilities::lookup(provider, model);
        assert!(
            caps.native_tools,
            "{provider}/{model} must reach native tools"
        );
        assert_eq!(
            caps.reasoning_tools_require_responses, requires_responses,
            "{provider}/{model}"
        );
        assert_eq!(
            effective_tool_api_mode(
                LlmApiMode::ChatCompletions,
                provider,
                &caps,
                &thinking,
                true
            ),
            if requires_responses {
                LlmApiMode::Responses
            } else {
                LlmApiMode::ChatCompletions
            },
            "{provider}/{model}"
        );
        assert_eq!(
            effective_tool_api_mode(
                LlmApiMode::ChatCompletions,
                provider,
                &caps,
                &ThinkingConfig::Effort {
                    level: ReasoningEffort::None,
                },
                true,
            ),
            LlmApiMode::ChatCompletions,
            "explicit none remains available on {provider}/{model}"
        );
        assert_eq!(
            effective_tool_api_mode(
                LlmApiMode::ChatCompletions,
                provider,
                &caps,
                &thinking,
                false
            ),
            LlmApiMode::ChatCompletions,
            "reasoning without native tools keeps the requested route"
        );
    }
}

#[test]
fn gpt_5_4_mini_tool_options_use_responses_unless_reasoning_is_disabled() {
    let _openai_key = ScopedEnvVar::set("OPENAI_API_KEY", "test-key");
    crate::llm::capabilities::clear_user_overrides();
    crate::llm_config::clear_user_overrides();
    for (effort, expected) in [
        (None, crate::llm::api::LlmApiMode::Responses),
        (Some("low"), crate::llm::api::LlmApiMode::Responses),
        (Some("none"), crate::llm::api::LlmApiMode::ChatCompletions),
    ] {
        let mut options = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::string("openai"),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::string("gpt-5.4-mini"),
            ),
            (crate::value::intern_key("tools"), one_tool_list()),
            (
                crate::value::intern_key("api_mode"),
                VmValue::string("chat_completions"),
            ),
        ]);
        if let Some(effort) = effort {
            options.insert(crate::value::intern_key("effort"), VmValue::string(effort));
        }
        let options = extract_with_options(options).expect("direct mini tool options");
        assert!(
            options
                .native_tools
                .as_ref()
                .is_some_and(|tools| !tools.is_empty()),
            "native tools must survive extraction"
        );
        assert_eq!(options.api_mode, expected, "effort={effort:?}");
        if let Some(effort) = effort {
            let expected = if effort == "none" {
                crate::llm::api::ReasoningEffort::None
            } else {
                crate::llm::api::ReasoningEffort::Low
            };
            assert!(matches!(
                options.thinking,
                crate::llm::api::ThinkingConfig::Effort { level } if level == expected
            ));
        }
    }
}

#[test]
fn gpt_5_6_reasoning_tools_auto_route_to_responses() {
    let _openai_key = ScopedEnvVar::set("OPENAI_API_KEY", "test-key");
    crate::llm::capabilities::clear_user_overrides();
    crate::llm_config::clear_user_overrides();

    let extract = |effort: Option<&str>, explicit_api: Option<&str>| {
        let mut options = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::string("openai"),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::string("gpt-5.6-terra"),
            ),
            (crate::value::intern_key("tools"), one_tool_list()),
        ]);
        if let Some(effort) = effort {
            options.insert(crate::value::intern_key("effort"), VmValue::string(effort));
        }
        if let Some(api) = explicit_api {
            options.insert(crate::value::intern_key("api_mode"), VmValue::string(api));
        }
        extract_with_options(options).expect("GPT-5.6 tool options")
    };

    assert_eq!(
        extract(None, None).api_mode,
        crate::llm::api::LlmApiMode::Responses,
        "omitting effort preserves the provider's reasoning default"
    );
    assert_eq!(
        extract(Some("medium"), None).api_mode,
        crate::llm::api::LlmApiMode::Responses
    );
    assert_eq!(
        extract(Some("none"), None).api_mode,
        crate::llm::api::LlmApiMode::ChatCompletions
    );
    assert_eq!(
        extract(Some("medium"), Some("chat_completions")).api_mode,
        crate::llm::api::LlmApiMode::Responses,
        "the provider compatibility constraint must prevent an invalid request"
    );
}

#[test]
fn equivalent_failover_filters_by_provider_tool_requirements() {
    let mut overlay = ProvidersConfig::default();
    for provider in ["mock", "plain-backup", "openai"] {
        overlay.providers.insert(
            provider.to_string(),
            test_provider(&format!("https://{provider}.example/v1")),
        );
    }
    overlay.models.insert(
        "tool-primary-model".to_string(),
        test_equivalent_model("mock", "provider-tool-equivalent-test"),
    );
    overlay.models.insert(
        "plain-backup-model".to_string(),
        test_equivalent_model("plain-backup", "provider-tool-equivalent-test"),
    );
    overlay.models.insert(
        "tool-backup-model".to_string(),
        test_equivalent_model("openai", "provider-tool-equivalent-test"),
    );
    crate::llm_config::set_user_overrides(Some(overlay));
    let capability_overlay = [
        "[[provider.mock]]",
        "model_match = \"tool-primary-model\"",
        "native_tools = true",
        "preferred_tool_format = \"native\"",
        "responses_api = true",
        "hosted_tools = [\"web_search\"]",
        "",
        "[[provider.plain-backup]]",
        "model_match = \"plain-backup-model\"",
        "native_tools = true",
        "preferred_tool_format = \"native\"",
        "responses_api = true",
        "",
        "[[provider.openai]]",
        "model_match = \"tool-backup-model\"",
        "native_tools = true",
        "preferred_tool_format = \"native\"",
        "responses_api = true",
        "hosted_tools = [\"web_search\"]",
    ]
    .join("\n");
    crate::llm::capabilities::set_user_overrides_toml(&capability_overlay)
        .expect("capability override");

    let opts = extract_with_options(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::string("mock"),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::string("tool-primary-model"),
        ),
        (
            crate::value::intern_key("api_mode"),
            VmValue::string("responses"),
        ),
        (
            crate::value::intern_key("provider_tools"),
            VmValue::list(vec![VmValue::string("web_search")]),
        ),
        (
            crate::value::intern_key("equivalent_failover"),
            VmValue::Bool(true),
        ),
    ]))
    .expect("options");

    let requirements = equivalent_failover_requirements_for_options(&opts);
    let candidates = crate::llm_config::equivalent_model_catalog_entries_for_requirements(
        "tool-primary-model",
        requirements.clone(),
    );
    assert!(
        candidates
            .iter()
            .any(|(id, model)| { id == "tool-backup-model" && model.provider == "openai" }),
        "requirements={requirements:?} candidates={candidates:?}"
    );
    assert!(candidates
        .iter()
        .all(|(id, model)| { id != "plain-backup-model" && model.provider != "plain-backup" }));

    crate::llm_config::clear_user_overrides();
    crate::llm::capabilities::clear_user_overrides();
}

#[test]
fn equivalent_failover_filters_routes_without_responses_api_support() {
    let mut overlay = ProvidersConfig::default();
    for provider in ["mock", "chat-backup", "openai"] {
        overlay.providers.insert(
            provider.to_string(),
            test_provider(&format!("https://{provider}.example/v1")),
        );
    }
    overlay.models.insert(
        "responses-primary-model".to_string(),
        test_equivalent_model("mock", "responses-equivalent-test"),
    );
    overlay.models.insert(
        "chat-backup-model".to_string(),
        test_equivalent_model("chat-backup", "responses-equivalent-test"),
    );
    overlay.models.insert(
        "responses-backup-model".to_string(),
        test_equivalent_model("openai", "responses-equivalent-test"),
    );
    crate::llm_config::set_user_overrides(Some(overlay));
    let capability_overlay = [
        "[[provider.mock]]",
        "model_match = \"responses-primary-model\"",
        "responses_api = true",
        "",
        "[[provider.chat-backup]]",
        "model_match = \"chat-backup-model\"",
        "responses_api = false",
        "",
        "[[provider.openai]]",
        "model_match = \"responses-backup-model\"",
        "responses_api = true",
    ]
    .join("\n");
    crate::llm::capabilities::set_user_overrides_toml(&capability_overlay)
        .expect("capability override");

    let opts = extract_with_options(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::string("mock"),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::string("responses-primary-model"),
        ),
        (
            crate::value::intern_key("api_mode"),
            VmValue::string("responses"),
        ),
        (
            crate::value::intern_key("equivalent_failover"),
            VmValue::Bool(true),
        ),
    ]))
    .expect("options");

    let requirements = equivalent_failover_requirements_for_options(&opts);
    assert!(requirements.responses_api);
    let candidates = crate::llm_config::equivalent_model_catalog_entries_for_requirements(
        "responses-primary-model",
        requirements,
    );
    assert!(candidates
        .iter()
        .any(|(id, model)| { id == "responses-backup-model" && model.provider == "openai" }));
    assert!(candidates
        .iter()
        .all(|(id, model)| { id != "chat-backup-model" && model.provider != "chat-backup" }));

    crate::llm_config::clear_user_overrides();
    crate::llm::capabilities::clear_user_overrides();
}
