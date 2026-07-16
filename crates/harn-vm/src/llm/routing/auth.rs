use super::{
    AttemptStatus, ChainLink, LlmCallOptions, RoutingAttempt, RoutingErrorSnapshot,
    RoutingPolicyConfig, VerifierOutcome,
};
use crate::llm::api::LlmRoutePolicy;

/// Effective per-link options: start from the base options, swap in
/// provider/model/timeout, and overlay the policy budget so per-call
/// preflight enforces it.
pub(super) fn link_options(
    base: &LlmCallOptions,
    policy: &RoutingPolicyConfig,
    link: &ChainLink,
) -> LlmCallOptions {
    let mut opts = base.clone();
    opts.provider = link.provider.clone();
    opts.model = link.model.clone();
    opts.region = link.region.clone();
    opts.api_key = String::new();
    opts.route_policy = LlmRoutePolicy::Always(format!("{}:{}", link.provider, link.model));
    opts.fallback_chain = Vec::new();
    opts.route_fallbacks = Vec::new();
    opts.routing_decision = None;
    // The executor owns chain dispatch; per-link calls must not recurse
    // back through `execute_llm_call`'s routing dispatch.
    opts.routing_policy = None;
    if let Some(timeout_ms) = link.timeout_ms.or(policy.failover.on_timeout_ms) {
        let secs = (timeout_ms / 1000).max(1);
        opts.timeout = Some(secs);
    }
    if let Some(envelope) = policy.budget.envelope() {
        let mut merged = opts.budget.clone().unwrap_or_default();
        if envelope.max_cost_usd.is_some() {
            merged.max_cost_usd = envelope.max_cost_usd;
        }
        if envelope.total_budget_usd.is_some() {
            merged.total_budget_usd = envelope.total_budget_usd;
        }
        opts.budget = Some(merged);
    }
    if let Some(overrides) = link.overrides.as_ref() {
        super::execution::apply_ladder_step_overrides(&mut opts, overrides);
    }
    opts
}

pub(super) fn link_options_with_auth(
    base: &LlmCallOptions,
    policy: &RoutingPolicyConfig,
    link: &ChainLink,
) -> Result<LlmCallOptions, RoutingErrorSnapshot> {
    let mut opts = link_options(base, policy, link);
    match crate::llm::resolve_api_key(&link.provider) {
        Ok(key) => {
            opts.api_key = key;
            Ok(opts)
        }
        Err(error) => Err(RoutingErrorSnapshot {
            category: "auth".to_string(),
            code: Some("route_unavailable".to_string()),
            reason: Some("missing_credentials".to_string()),
            attempt_count: Some(0),
            message: error.to_string(),
            status: None,
        }),
    }
}

pub(super) fn skipped_attempt_record(
    attempt_no: usize,
    link: &ChainLink,
    label: &str,
    error: RoutingErrorSnapshot,
) -> RoutingAttempt {
    RoutingAttempt {
        index: attempt_no,
        provider: link.provider.clone(),
        model: link.model.clone(),
        label: label.to_string(),
        status: AttemptStatus::Skipped,
        duration_ms: 0,
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        error: Some(error),
        verifier_signals: Vec::new(),
        verifier_outcome: None::<VerifierOutcome>,
    }
}
