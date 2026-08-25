use crate::llm_config::{AuthEnv, ModelAvailability, ModelDef, ProviderDef};
use crate::value::{VmError, VmValue};

pub(super) fn extract_with_options(
    opts: crate::value::DictMap,
) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    super::extract::extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello")),
        VmValue::Nil,
        VmValue::dict(opts),
    ])
}

pub(super) fn one_tool_list() -> VmValue {
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

pub(super) struct ScopedEnvVar {
    key: &'static str,
    previous: Option<String>,
}

impl ScopedEnvVar {
    pub(super) fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for ScopedEnvVar {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
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
    ModelDef {
        name: format!("{provider} equivalent model"),
        display_name: None,
        blurb: None,
        provider: provider.to_string(),
        context_window,
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
        serving_tiers: Vec::new(),
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
        completion_review: None,
        released: None,
        row_kind: None,
        current_snapshot: None,
        embedding_dim: None,
        embedding_max_tokens: None,
    }
}
