//! Runtime wiring for catalog-declared synchronous serving tiers.
//!
//! The catalog (`[llm.models.<id>].serving_tiers`) is the single source of
//! truth for provider request knobs such as Anthropic `speed = "fast"`,
//! OpenAI/Gemini `service_tier = "priority"`, and discounted best-effort
//! tiers such as Gemini Flex. Batch APIs are intentionally separate async
//! capabilities; this module only handles per-request synchronous tiers.
//!
//! The existing `fast: true` call option selects the tier whose id is `fast`.
//! That preserves the ergonomic API while removing the bespoke fast-mode
//! catalog shape.

use crate::llm_config::{model_catalog_entry, ServingTierDef};

pub(crate) const FAST_TIER_ID: &str = "fast";

/// Catalog lifecycle status that disqualifies a serving tier from use:
/// the provider has announced its removal and `param=value` either errors
/// or silently degrades to standard serving.
const DEPRECATED_STATUS: &str = "deprecated";

/// Resolve a named serving tier from the catalog, if any.
pub(crate) fn lookup(model: &str, tier_id: &str) -> Option<ServingTierDef> {
    model_catalog_entry(model).and_then(|entry| {
        entry
            .serving_tiers
            .into_iter()
            .find(|tier| tier.id == tier_id)
    })
}

/// Resolve the model's accelerated `fast` tier from the catalog, if any.
pub(crate) fn fast_tier(model: &str) -> Option<ServingTierDef> {
    lookup(model, FAST_TIER_ID)
}

/// Whether a serving tier is currently usable. A `deprecated` tier is
/// still described in the catalog (so callers can migrate) but must not be
/// engaged on new requests.
pub(crate) fn is_usable(tier: &ServingTierDef) -> bool {
    tier.status.as_deref() != Some(DEPRECATED_STATUS)
}

/// Outcome of validating a serving-tier request against the catalog.
pub(crate) enum ServingTierGate {
    /// The model offers a usable fast-mode tier; engage it. The catalog
    /// metadata is re-read by the provider body builder, so the variant
    /// carries no payload.
    Usable,
    /// The model has no fast-mode tier at all.
    Unsupported,
    /// The model's fast-mode tier is deprecated; carries the catalog note
    /// so the diagnostic can point at the migration target.
    Deprecated { note: Option<String> },
}

/// Classify a `fast: true` request for the resolved model.
pub(crate) fn fast_gate(model: &str) -> ServingTierGate {
    match fast_tier(model) {
        None => ServingTierGate::Unsupported,
        Some(tier) if !is_usable(&tier) => ServingTierGate::Deprecated { note: tier.note },
        Some(tier) if tier.request.is_none() => ServingTierGate::Unsupported,
        Some(_) => ServingTierGate::Usable,
    }
}

/// Inject the `fast` serving-tier request knob into an already-built provider body.
/// No-op when `fast` is false or the model has no usable fast-mode tier, so
/// it is safe to call unconditionally from every provider body builder.
pub(crate) fn apply_fast_request_knob(body: &mut serde_json::Value, model: &str, fast: bool) {
    if !fast {
        return;
    }
    let Some(tier) = fast_tier(model).filter(is_usable) else {
        return;
    };
    let Some(request) = tier.request else {
        return;
    };
    if let Some(object) = body.as_object_mut() {
        object.insert(request.param, serde_json::Value::String(request.value));
    }
}

/// The Anthropic-style beta header required to engage fast mode for `model`,
/// when one is declared. Returns `None` for providers (e.g. OpenAI) whose
/// fast tier needs no beta gate, or when `fast` is false / the tier is
/// deprecated.
pub(crate) fn beta_header(model: &str, fast: bool) -> Option<String> {
    if !fast {
        return None;
    }
    fast_tier(model)
        .filter(is_usable)
        .and_then(|tier| tier.request)
        .and_then(|request| request.beta_header)
}

/// Whether a provider response indicates the request was actually served at
/// the fast tier. Providers echo the knob (`speed` / `service_tier`) either
/// at the top level or inside `usage`; downgrades on capacity pressure echo
/// a different value (e.g. `default`), so this is the authoritative signal
/// for billing rather than the request intent.
///
/// `obj` may be a whole response, a streaming `message_start.message`, or a
/// final streaming usage chunk — anything that carries the echoed knob at its
/// root or under `usage`.
pub(crate) fn served_fast(model: &str, obj: &serde_json::Value) -> bool {
    let Some(tier) = fast_tier(model) else {
        return false;
    };
    let Some(request) = tier.request else {
        return false;
    };
    let matches = |scope: &serde_json::Value| {
        scope.get(&request.param).and_then(|v| v.as_str()) == Some(request.value.as_str())
    };
    matches(obj) || obj.get("usage").map(matches).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_resolves_anthropic_speed_knob() {
        let fast = fast_tier("claude-opus-4-8").expect("opus 4.8 advertises fast mode");
        let request = fast.request.as_ref().expect("fast tier has request knob");
        assert_eq!(request.param, "speed");
        assert_eq!(request.value, "fast");
        assert_eq!(request.beta_header.as_deref(), Some("fast-mode-2026-02-01"));
        assert!(is_usable(&fast));
    }

    #[test]
    fn gate_rejects_unsupported_and_deprecated() {
        assert!(matches!(fast_gate("gpt-4o"), ServingTierGate::Unsupported));
        // Opus 4.6's fast tier has been removed from the catalog after
        // Anthropic's June 29, 2026 removal date.
        assert!(matches!(
            fast_gate("claude-opus-4-6"),
            ServingTierGate::Unsupported
        ));

        let overlay = crate::llm_config::parse_config_toml(concat!(
            "[models.\"test-deprecated-fast\"]\n",
            "name = \"Deprecated Fast Test\"\n",
            "provider = \"test\"\n",
            "context_window = 128000\n",
            "serving_tiers = [\n",
            "  { id = \"fast\", mode = \"synchronous\", economics = \"premium\", ",
            "request = { param = \"speed\", value = \"fast\" }, ",
            "status = \"deprecated\", note = \"removed\" },\n",
            "]\n",
        ))
        .expect("test catalog overlay");
        crate::llm_config::set_user_overrides(Some(overlay));
        assert!(matches!(
            fast_gate("test-deprecated-fast"),
            ServingTierGate::Deprecated { .. }
        ));
        crate::llm_config::clear_user_overrides();

        assert!(matches!(fast_gate("gpt-5.5"), ServingTierGate::Usable));
    }

    #[test]
    fn gate_rejects_fast_tier_without_request_knob() {
        let overlay = crate::llm_config::parse_config_toml(concat!(
            "[models.\"test-fast-without-knob\"]\n",
            "name = \"Missing Fast Knob Test\"\n",
            "provider = \"test\"\n",
            "context_window = 128000\n",
            "serving_tiers = [\n",
            "  { id = \"fast\", mode = \"synchronous\", economics = \"premium\" },\n",
            "]\n",
        ))
        .expect("test catalog overlay");
        crate::llm_config::set_user_overrides(Some(overlay));
        assert!(matches!(
            fast_gate("test-fast-without-knob"),
            ServingTierGate::Unsupported
        ));
        crate::llm_config::clear_user_overrides();
    }

    #[test]
    fn apply_fast_request_knob_sets_provider_field() {
        let mut anthropic = serde_json::json!({"model": "claude-opus-4-8"});
        apply_fast_request_knob(&mut anthropic, "claude-opus-4-8", true);
        assert_eq!(anthropic["speed"], serde_json::json!("fast"));

        let mut openai = serde_json::json!({"model": "gpt-5.5"});
        apply_fast_request_knob(&mut openai, "gpt-5.5", true);
        assert_eq!(openai["service_tier"], serde_json::json!("fast"));
    }

    #[test]
    fn apply_fast_request_knob_is_noop_when_off_or_unsupported() {
        let mut body = serde_json::json!({"model": "claude-opus-4-8"});
        apply_fast_request_knob(&mut body, "claude-opus-4-8", false);
        assert!(body.get("speed").is_none());

        let mut unsupported = serde_json::json!({"model": "gpt-4o"});
        apply_fast_request_knob(&mut unsupported, "gpt-4o", true);
        assert!(unsupported.get("service_tier").is_none());
    }

    #[test]
    fn beta_header_only_for_beta_gated_tiers() {
        assert_eq!(
            beta_header("claude-opus-4-8", true).as_deref(),
            Some("fast-mode-2026-02-01")
        );
        // OpenAI's service_tier needs no beta header.
        assert_eq!(beta_header("gpt-5.5", true), None);
        assert_eq!(beta_header("claude-opus-4-8", false), None);
    }

    #[test]
    fn served_fast_reads_echo_at_root_or_in_usage() {
        // Anthropic echoes `speed` inside usage.
        let anthropic = serde_json::json!({"usage": {"speed": "fast", "output_tokens": 10}});
        assert!(served_fast("claude-opus-4-8", &anthropic));

        // OpenAI echoes `service_tier` at the top level.
        let openai = serde_json::json!({"service_tier": "fast", "usage": {"completion_tokens": 5}});
        assert!(served_fast("gpt-5.5", &openai));

        // A downgrade echoes a different value.
        let downgraded = serde_json::json!({"service_tier": "default"});
        assert!(!served_fast("gpt-5.5", &downgraded));

        // Models without a fast tier never report served-fast.
        assert!(!served_fast(
            "gpt-4o",
            &serde_json::json!({"service_tier": "fast"})
        ));
    }
}
