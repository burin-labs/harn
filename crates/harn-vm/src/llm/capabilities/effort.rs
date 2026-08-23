//! Effort-capability derivation for a capability rule.
//!
//! `thinking_modes` is the single owner. Three spellings in a fragment can
//! assert that a route takes an effort ladder -- `thinking_modes` containing
//! `effort`, the legacy `reasoning_effort_supported` flag, and a non-empty
//! `reasoning_effort_levels` -- and the other two are absorbed here rather than
//! compared. Absorbing makes the contradiction unrepresentable instead of
//! merely detectable: a fragment used to be able to declare a three-rung ladder
//! while leaving `effort` out of `thinking_modes`, and the two halves of Harn
//! then disagreed about the same route. The reasoning policy read the flag and
//! produced an `Effort` config; the option validator read the modes and refused
//! the config the policy had just built, so the route was unusable through
//! either entry point.
//!
//! Each extra spelling can only ADD `effort`, never remove it. Declaring a
//! ladder or setting the flag is an affirmative claim; the way to say a route
//! does not take effort is to leave `effort` out of `thinking_modes` and not
//! make the claim elsewhere.

use super::rule::ProviderRule;

/// The reasoning-control shapes a route accepts.
pub(super) fn rule_thinking_modes(rule: &ProviderRule) -> Vec<String> {
    let mut modes = rule.thinking_modes.clone().unwrap_or_else(|| {
        if rule.thinking.unwrap_or(false) {
            vec!["enabled".to_string()]
        } else {
            Vec::new()
        }
    });
    let claims_effort = rule.reasoning_effort_supported.unwrap_or(false)
        || rule
            .reasoning_effort_levels
            .as_ref()
            .is_some_and(|levels| !levels.is_empty());
    if claims_effort && !modes.iter().any(|mode| mode == "effort") {
        modes.push("effort".to_string());
    }
    modes
}

/// Whether the route takes a reasoning-effort ladder.
///
/// Derived from the resolved modes, so it cannot disagree with them. Consumers
/// used to hedge with `caps_supports(caps, "effort") || caps.reasoning_effort_supported`
/// precisely because the two could differ; that hedge is now redundant.
pub(super) fn rule_reasoning_effort_supported(rule: &ProviderRule) -> bool {
    rule_thinking_modes(rule)
        .iter()
        .any(|mode| mode == "effort")
}
