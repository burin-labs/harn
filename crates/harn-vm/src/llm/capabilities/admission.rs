use super::overrides::current_user_overrides;
use super::rule::resolved_rule_and_defaults;

/// Return authored support without treating unknown custom routes as capable.
fn declared_portable_option_support(
    user: Option<&super::model::CapabilitiesFile>,
    builtin: &super::model::CapabilitiesFile,
    provider: &str,
    model: &str,
    option: PortableOption,
) -> (Option<bool>, Option<Vec<String>>) {
    let (rule, defaults) = resolved_rule_and_defaults(user, builtin, provider, model);
    let rule = rule.as_ref();
    let supported = match option {
        PortableOption::Temperature => rule
            .and_then(|rule| rule.temperature_supported)
            .or(defaults.temperature_supported),
        PortableOption::TopP => rule
            .and_then(|rule| rule.top_p_supported)
            .or(defaults.top_p_supported),
        PortableOption::TopK => rule
            .and_then(|rule| rule.top_k_supported)
            .or(defaults.top_k_supported),
        PortableOption::Seed => rule
            .and_then(|rule| rule.seed_supported)
            .or(defaults.seed_supported),
        PortableOption::FrequencyPenalty => rule
            .and_then(|rule| rule.frequency_penalty_supported)
            .or(defaults.frequency_penalty_supported),
        PortableOption::PresencePenalty => rule
            .and_then(|rule| rule.presence_penalty_supported)
            .or(defaults.presence_penalty_supported),
        PortableOption::Stop => rule
            .and_then(|rule| rule.stop_supported)
            .or(defaults.stop_supported),
        PortableOption::Logprobs
        | PortableOption::LogitBias
        | PortableOption::MinP
        | PortableOption::RepetitionPenalty
        | PortableOption::Prediction
        | PortableOption::Verbosity
        | PortableOption::Mirostat => Some(
            rule.and_then(|rule| rule.advanced_generation_options.as_ref())
                .or(defaults.advanced_generation_options.as_ref())
                .is_some_and(|options| options.contains(&option)),
        ),
        PortableOption::ParallelToolCalls => rule
            .and_then(|rule| rule.supports_parallel_tool_calls)
            .or(defaults.supports_parallel_tool_calls),
        PortableOption::Cache | PortableOption::PromptCacheTtl => {
            rule.and_then(|rule| rule.prompt_caching)
        }
    };
    let values = (option == PortableOption::PromptCacheTtl).then(|| {
        rule.and_then(|rule| rule.prompt_cache_ttls.clone())
            .or_else(|| defaults.prompt_cache_ttls.clone())
            .unwrap_or_default()
    });
    (supported, values)
}

pub use harn_provider_catalog::capabilities::PortableOption;

/// A caller requested a portable option that the resolved route cannot
/// represent. The route and option stay structured so every projection can
/// render its own diagnostic without duplicating capability policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityAdmissionError {
    pub provider: String,
    pub model: String,
    pub option: PortableOption,
    /// Whether the route rejects this otherwise-supported option only while
    /// reasoning is enabled.
    pub reasoning_enabled: bool,
    pub requested_value: Option<String>,
    pub supported_values: Vec<String>,
}

impl std::fmt::Display for CapabilityAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "option `{}`{} is not supported{} by `{}` (provider `{}`).",
            self.option.name(),
            self.requested_value
                .as_deref()
                .map(|value| format!(" value `{value}`"))
                .unwrap_or_default(),
            if self.reasoning_enabled {
                " while reasoning is enabled"
            } else {
                ""
            },
            self.model,
            self.provider,
        )?;
        if !self.supported_values.is_empty() {
            write!(
                f,
                " Supported values: {}.",
                self.supported_values.join(", ")
            )?;
        }
        write!(
            f,
            " Remove it, choose a compatible route, or move a provider-native control below `provider_options.{}`. See `harn provider catalog matrix` for compatibility.",
            self.provider
        )
    }
}

/// Admit one caller-selected portable option against the canonical capability
/// registry. Generation fields remain open-world because adapters can project
/// them unchanged. Cache controls require an authored capability because Harn
/// must choose a provider-specific lowering.
pub fn admit_portable_option(
    provider: &str,
    model: &str,
    option: PortableOption,
) -> Result<(), CapabilityAdmissionError> {
    admit_portable_option_with_effort(provider, model, option, None)
}

/// Admit a statically known option and its explicitly authored effort.
/// Omission does not satisfy a route's conditional support declaration.
pub fn admit_portable_option_with_effort(
    provider: &str,
    model: &str,
    option: PortableOption,
    effort: Option<super::ReasoningEffort>,
) -> Result<(), CapabilityAdmissionError> {
    let thinking = effort.map_or(crate::llm::api::ThinkingConfig::Disabled, |level| {
        crate::llm::api::ThinkingConfig::Effort { level }
    });
    admit_portable_option_for_thinking(provider, model, &thinking, option, None)
}

/// Admit a caller-selected portable option for one resolved call. Unlike the
/// cross-crate static preflight above, this seam can inspect the final thinking
/// configuration after routing and policy have selected it.
pub(crate) fn admit_portable_option_for_thinking(
    provider: &str,
    model: &str,
    thinking: &crate::llm::api::ThinkingConfig,
    option: PortableOption,
    provider_contract_probe: Option<PortableOption>,
) -> Result<(), CapabilityAdmissionError> {
    debug_assert_ne!(option, PortableOption::PromptCacheTtl);
    // Same substituted route the `Capabilities` lookup resolves, so a mocked
    // call cannot admit here and refuse there (harn#7693, harn#8119).
    let (provider, model) = super::lookup::effective_capability_route(provider, model);
    if !crate::llm::provider_contract_probe::catalog_may_shape_requested_portable_option(
        provider_contract_probe,
        option,
    ) {
        return Ok(());
    }
    let user = current_user_overrides();
    let builtin = super::lookup::builtin();
    let (supported, _) =
        declared_portable_option_support(user.as_ref(), builtin, provider, model, option);
    let caps = super::lookup::lookup(provider, model);
    let effort_requirement = caps.portable_option_effort_support.get(&option);
    let explicit_effort_matches = effort_requirement.is_some_and(|levels| {
        matches!(thinking, crate::llm::api::ThinkingConfig::Effort { level }
            if levels.contains(level)
                && caps.reasoning_effort_supported
                && caps.reasoning_effort_levels.iter().any(|supported| supported == level.as_str())
                && (*level != super::ReasoningEffort::None || caps.reasoning_none_supported))
    });
    let rejected_while_reasoning =
        thinking.is_enabled() && caps.reasoning_excluded_portable_options.contains(&option);
    let requires_authored_support = matches!(
        option,
        PortableOption::Cache
            | PortableOption::Logprobs
            | PortableOption::LogitBias
            | PortableOption::MinP
            | PortableOption::RepetitionPenalty
            | PortableOption::Prediction
            | PortableOption::Verbosity
            | PortableOption::Mirostat
            | PortableOption::ParallelToolCalls
    );
    let admitted = if effort_requirement.is_some() {
        explicit_effort_matches
    } else {
        supported == Some(true) || (supported.is_none() && !requires_authored_support)
    };
    if !rejected_while_reasoning && admitted {
        return Ok(());
    }
    Err(CapabilityAdmissionError {
        provider: provider.to_string(),
        model: model.to_string(),
        option,
        reasoning_enabled: rejected_while_reasoning,
        requested_value: None,
        supported_values: Vec::new(),
    })
}

/// Admit one explicit prompt-cache TTL. A route that declares prompt caching
/// but no selectable TTL values cannot represent caller-selected TTL intent.
/// A route with no cache facts is rejected because Harn has no sound TTL
/// lowering to project for it.
pub fn admit_prompt_cache_ttl(
    provider: &str,
    model: &str,
    ttl: &str,
) -> Result<(), CapabilityAdmissionError> {
    let (provider, model) = super::lookup::effective_capability_route(provider, model);
    let user = current_user_overrides();
    let builtin = super::lookup::builtin();
    let (cache_supported, supported_values) = declared_portable_option_support(
        user.as_ref(),
        builtin,
        provider,
        model,
        PortableOption::PromptCacheTtl,
    );
    match cache_supported {
        Some(true)
            if supported_values
                .as_ref()
                .is_some_and(|values| values.iter().any(|value| value == ttl)) =>
        {
            return Ok(())
        }
        Some(true) | Some(false) | None => {}
    }
    Err(CapabilityAdmissionError {
        provider: provider.to_string(),
        model: model.to_string(),
        option: PortableOption::PromptCacheTtl,
        reasoning_enabled: false,
        requested_value: Some(ttl.to_string()),
        supported_values: supported_values.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::capabilities::{clear_user_overrides, set_user_overrides_toml};

    #[test]
    fn rejects_declared_gap_and_keeps_unknown_routes_open_world() {
        let rejected = admit_portable_option("moonshot", "kimi-k3", PortableOption::Temperature)
            .expect_err("Kimi K3 rejects caller-selected temperature");
        assert_eq!(rejected.option, PortableOption::Temperature);
        assert!(rejected.to_string().contains("provider_options.moonshot"));

        assert!(
            admit_portable_option("my-proxy", "custom-model", PortableOption::Temperature,).is_ok()
        );
    }

    #[test]
    fn gemini_interactions_routes_reject_unrepresentable_penalties() {
        for model in [
            "gemini-3.6-flash",
            "gemini-3.7-pro",
            "gemini-3.5-flash-lite",
            "models/gemini-3.5-flash-lite",
        ] {
            for option in [
                PortableOption::FrequencyPenalty,
                PortableOption::PresencePenalty,
            ] {
                let error = admit_portable_option("gemini", model, option)
                    .expect_err("Interactions has no penalty wire field");
                assert_eq!(error.option, option, "unexpected admission for {model}");
            }
        }
    }

    #[test]
    fn cache_and_ttl_admission_require_authored_lowering() {
        set_user_overrides_toml(
            r#"
[[provider.test-provider]]
model_match = "no-cache"
prompt_caching = false

[[provider.test-provider]]
model_match = "cache-with-ttl"
prompt_caching = true
prompt_cache_ttls = ["5m", "1h"]
"#,
        )
        .unwrap();

        let cache = admit_portable_option("test-provider", "no-cache", PortableOption::Cache)
            .expect_err("the synthetic route declares prompt caching unsupported");
        assert_eq!(cache.option, PortableOption::Cache);

        admit_prompt_cache_ttl("test-provider", "cache-with-ttl", "1h")
            .expect("the synthetic route supports the one-hour TTL");
        let unsupported = admit_prompt_cache_ttl("test-provider", "cache-with-ttl", "2h")
            .expect_err("the synthetic route rejects an unlisted TTL");
        assert_eq!(unsupported.requested_value.as_deref(), Some("2h"));
        assert_eq!(unsupported.supported_values, ["5m", "1h"]);

        let unknown = admit_prompt_cache_ttl("my-proxy", "custom-model", "1h")
            .expect_err("unknown custom routes have no sound TTL lowering");
        assert_eq!(unknown.option, PortableOption::PromptCacheTtl);
        clear_user_overrides();
    }

    #[test]
    fn advanced_generation_controls_require_an_authored_wire_lowering() {
        for option in [PortableOption::Logprobs, PortableOption::LogitBias] {
            admit_portable_option("openai", "gpt-4o", option)
                .expect("OpenAI Chat has an authored lowering");
        }
        admit_portable_option("ollama", "qwen3", PortableOption::Mirostat)
            .expect("Ollama has a native Mirostat lowering");
        admit_portable_option(
            "anthropic",
            "claude-sonnet-4-20250514",
            PortableOption::ParallelToolCalls,
        )
        .expect("Anthropic has a typed disable_parallel_tool_use lowering");

        for (provider, model, option) in [
            ("gemini", "gemini-3.6-flash", PortableOption::Logprobs),
            ("groq", "llama-3.3-70b-versatile", PortableOption::Logprobs),
            (
                "anthropic",
                "claude-sonnet-4-20250514",
                PortableOption::LogitBias,
            ),
            ("gemini", "gemini-2.5-flash", PortableOption::LogitBias),
            (
                "gemini",
                "gemini-2.5-flash",
                PortableOption::ParallelToolCalls,
            ),
            ("my-proxy", "custom-model", PortableOption::Mirostat),
        ] {
            admit_portable_option(provider, model, option)
                .expect_err("an unowned wire projection must be rejected");
        }
    }
}
