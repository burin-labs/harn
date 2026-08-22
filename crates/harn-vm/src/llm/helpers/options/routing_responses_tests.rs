use super::routing_test_support::{
    extract_with_options, one_tool_list, test_equivalent_model, test_provider, ScopedEnvVar,
};
use super::*;
use crate::llm::helpers::options::routing::equivalent_failover_requirements_for_options;
use crate::llm_config::ProvidersConfig;

#[test]
fn gpt_5_6_reasoning_tools_auto_route_to_responses() {
    let _openai_key = ScopedEnvVar::set("OPENAI_API_KEY", "test-key");
    crate::llm::capabilities::clear_user_overrides();
    crate::llm_config::clear_user_overrides();

    let extract = |effort: Option<&str>, explicit_api: Option<&str>| {
        let mut options = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("openai")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("gpt-5.6-terra")),
            ),
            (crate::value::intern_key("tools"), one_tool_list()),
        ]);
        if let Some(effort) = effort {
            options.insert(
                crate::value::intern_key("effort"),
                VmValue::String(arcstr::ArcStr::from(effort)),
            );
        }
        if let Some(api) = explicit_api {
            options.insert(
                crate::value::intern_key("api_mode"),
                VmValue::String(arcstr::ArcStr::from(api)),
            );
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
            VmValue::String(arcstr::ArcStr::from("mock")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("tool-primary-model")),
        ),
        (
            crate::value::intern_key("api_mode"),
            VmValue::String(arcstr::ArcStr::from("responses")),
        ),
        (
            crate::value::intern_key("provider_tools"),
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("web_search"),
            )])),
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
            VmValue::String(arcstr::ArcStr::from("mock")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("responses-primary-model")),
        ),
        (
            crate::value::intern_key("api_mode"),
            VmValue::String(arcstr::ArcStr::from("responses")),
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
