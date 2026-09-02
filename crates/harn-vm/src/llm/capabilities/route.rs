//! Route resolution for capability rules.
//!
//! One seam so the `Capabilities` lookup and the portable-option admission
//! gate can never resolve the same (provider, model) pair differently
//! (harn#7693).

use super::rule::{
    absorb_layer_matches, merged_provider_defaults, resolve_rule_chain, RuleResolution,
};
use super::{CapabilitiesFile, ProviderDefaults};

/// The rule lists `mock` borrows, in order, when it has no rule of its own for
/// the model. `mock` spoofs whichever shape the model id names, which is a
/// name-shape fan-out rather than the single-parent `provider_family` edge the
/// normal chain walks, so it needs its own layer sequence.
const MOCK_SPOOF_LAYERS: [&str; 3] = ["anthropic", "openai", "gemini"];

/// The one route-resolution seam. Every consumer of capability facts — the
/// `Capabilities` lookup and the portable-option admission gate alike — goes
/// through here, so a route cannot resolve one way for "does this model
/// support native tools" and another way for "does this model support prompt
/// caching" (harn#7693).
///
/// Returns the resolution, its effective defaults, and the layer whose rule
/// matched, so a caller can apply a layer-specific pin.
pub(super) fn resolve_route(
    user: Option<&CapabilitiesFile>,
    builtin: &CapabilitiesFile,
    provider: &str,
    model: &str,
) -> (RuleResolution, ProviderDefaults, Option<&'static str>) {
    if provider == "mock" {
        // `mock`'s own rows first, so the double can declare the surface a
        // capable route declares (and so a `[[provider.mock]]` override in
        // `harn.toml` is reachable at all). Then the spoof layers, Anthropic
        // first, so `mock` + `claude-opus-4-7` keeps resolving to the
        // Anthropic capability row.
        for layer in std::iter::once("mock").chain(MOCK_SPOOF_LAYERS) {
            let defaults = merged_provider_defaults(user, builtin, layer);
            let mut resolution = RuleResolution::default();
            absorb_layer_matches(user, builtin, layer, model, &mut resolution);
            if resolution.merged.is_some() {
                return (resolution, defaults, Some(layer));
            }
        }
        return (RuleResolution::default(), ProviderDefaults::default(), None);
    }
    let (resolution, defaults) = resolve_rule_chain(user, builtin, provider, model);
    (resolution, defaults, None)
}
