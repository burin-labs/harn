use super::overrides::current_user_overrides;
use super::rule::declared_portable_option_support;

/// Portable generation options whose availability is declared by the
/// provider capability registry.
///
/// This enum is the shared vocabulary for runtime admission, routing-step
/// overrides, and static preflight. Provider-specific escape hatches remain
/// below `provider_options.<provider>` and never enter this portable lane.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PortableOption {
    Temperature,
    TopP,
    TopK,
    Seed,
    FrequencyPenalty,
    PresencePenalty,
    Stop,
    Cache,
    PromptCacheTtl,
}

impl PortableOption {
    /// Options whose non-`nil` presence is enough to prove caller intent.
    /// Cache needs a true value and prompt-cache TTL needs its selected value,
    /// so their callers use the dedicated admission paths below.
    pub const PRESENCE_DRIVEN: [Self; 7] = [
        Self::Temperature,
        Self::TopP,
        Self::TopK,
        Self::Seed,
        Self::FrequencyPenalty,
        Self::PresencePenalty,
        Self::Stop,
    ];

    pub const ALL: [Self; 9] = [
        Self::Temperature,
        Self::TopP,
        Self::TopK,
        Self::Seed,
        Self::FrequencyPenalty,
        Self::PresencePenalty,
        Self::Stop,
        Self::Cache,
        Self::PromptCacheTtl,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Temperature => "temperature",
            Self::TopP => "top_p",
            Self::TopK => "top_k",
            Self::Seed => "seed",
            Self::FrequencyPenalty => "frequency_penalty",
            Self::PresencePenalty => "presence_penalty",
            Self::Stop => "stop",
            Self::Cache => "cache",
            Self::PromptCacheTtl => "prompt_cache_ttl",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|option| option.name() == name)
    }
}

/// A caller requested a portable option that the resolved route cannot
/// represent. The route and option stay structured so every projection can
/// render its own diagnostic without duplicating capability policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityAdmissionError {
    pub provider: String,
    pub model: String,
    pub option: PortableOption,
    pub requested_value: Option<String>,
    pub supported_values: Vec<String>,
}

impl std::fmt::Display for CapabilityAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "option `{}`{} is not supported by `{}` (provider `{}`).",
            self.option.name(),
            self.requested_value
                .as_deref()
                .map(|value| format!(" value `{value}`"))
                .unwrap_or_default(),
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
    debug_assert_ne!(option, PortableOption::PromptCacheTtl);
    let user = current_user_overrides();
    let builtin = super::lookup::builtin();
    let (supported, _) =
        declared_portable_option_support(user.as_ref(), builtin, provider, model, option);
    let requires_authored_support = option == PortableOption::Cache;
    if supported == Some(true) || (supported.is_none() && !requires_authored_support) {
        return Ok(());
    }
    Err(CapabilityAdmissionError {
        provider: provider.to_string(),
        model: model.to_string(),
        option,
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
}
