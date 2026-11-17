pub(super) use crate::llm::test_env::ScopedEnvVar;
use crate::llm_config::{AuthEnv, ModelDef, ProviderDef};
use crate::value::{VmError, VmValue};

pub(super) fn extract_with_options(
    opts: crate::value::DictMap,
) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    super::extract::extract_llm_options(&[
        VmValue::string("hello"),
        VmValue::Nil,
        VmValue::dict(opts),
    ])
}

pub(super) fn one_tool_list() -> VmValue {
    VmValue::list(vec![VmValue::Dict(std::sync::Arc::new(
        crate::value::DictMap::from_iter([
            (crate::value::intern_key("name"), VmValue::string("lookup")),
            (
                crate::value::intern_key("description"),
                VmValue::string("Look something up"),
            ),
            (
                crate::value::intern_key("parameters"),
                VmValue::dict(crate::value::DictMap::new()),
            ),
        ]),
    ))])
}

pub(super) fn test_provider(url: &str) -> ProviderDef {
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

pub(super) fn test_equivalent_model(provider: &str, group: &str) -> ModelDef {
    test_equivalent_model_with_context(provider, group, 32_000)
}

pub(super) fn test_equivalent_model_with_context(
    provider: &str,
    group: &str,
    context_window: u64,
) -> ModelDef {
    crate::test_support::decode_fixture::<ModelDef>(serde_json::json!({
        "name": format!("{provider} equivalent model"),
        "provider": provider.to_string(),
        "context_window": context_window,
        "equivalence_group": Some(group.to_string()),
        "tier": Some("mid".to_string()),
        "open_weight": Some(true),
        "family": Some("test-equivalent-family".to_string())
    }))
}
