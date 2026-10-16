//! Runtime identity and diagnostic adapter for the pure capability policy.

use harn_provider_catalog::capabilities::{CapabilityResolution, CapabilityRoute, CapabilityRules};

use super::{Capabilities, CapabilitiesFile, ProviderDefaults};

pub use harn_provider_catalog::capabilities::ProviderRule;
pub(super) use harn_provider_catalog::capabilities::{
    rule_structured_output, rule_structured_output_mode, rule_thinking_block_style,
    rule_thinking_modes, rule_tool_mode_parity, rule_vision, MatchedCapabilityRule,
};

pub(super) fn resolve_with_features(
    provider: &str,
    model: &str,
    builtin: &CapabilitiesFile,
    user: Option<&CapabilitiesFile>,
    responses_api: bool,
) -> CapabilityResolution {
    let resolved = CapabilityRules::new(builtin, user).resolve(CapabilityRoute {
        provider,
        model,
        responses_api,
    });
    if provider != "mock" && !resolved.matched() && !resolved.has_defaults() {
        super::diagnostics::warn_unmatched_route_once(provider, model);
    }
    resolved
}

pub(super) fn lookup_with_match(
    provider: &str,
    model: &str,
    builtin: &CapabilitiesFile,
    user: Option<&CapabilitiesFile>,
) -> (Capabilities, bool) {
    let resolved = resolve_with_features(provider, model, builtin, user, false);
    let matched = resolved.matched();
    (resolved.capabilities(), matched)
}

pub(super) fn first_matching_rule(
    user: Option<&CapabilitiesFile>,
    builtin: &CapabilitiesFile,
    provider: &str,
    model: &str,
) -> Option<MatchedCapabilityRule> {
    CapabilityRules::new(builtin, user)
        .resolve(CapabilityRoute {
            provider,
            model,
            responses_api: false,
        })
        .matched_rule
}

pub(super) fn resolved_rule_and_defaults(
    user: Option<&CapabilitiesFile>,
    builtin: &CapabilitiesFile,
    provider: &str,
    model: &str,
) -> (Option<ProviderRule>, ProviderDefaults) {
    let model = crate::llm_config::capability_model_id(provider, model);
    let resolved = CapabilityRules::new(builtin, user).resolve(CapabilityRoute {
        provider,
        model: &model,
        responses_api: false,
    });
    (
        resolved.matched_rule.map(|matched| matched.rule),
        resolved.defaults,
    )
}
