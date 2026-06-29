//! Runtime wiring for the catalog's accelerated-serving ("fast mode") tier.
//!
//! The catalog (`[llm.models.<id>].fast_mode`) is the single source of truth:
//! it carries the per-provider request knob (`speed` for Anthropic,
//! `service_tier` for OpenAI), the value to send, the beta header (if any),
//! the premium pricing, and the lifecycle status. This module reads that
//! metadata so the live request path never hardcodes a provider quirk.
//!
//! Fast mode is opt-in (`llm_call(..., { fast: true })`) and off by default.
//! See #2616 for the catalog metadata and #2619 for this runtime half.

use crate::llm_config::{model_catalog_entry, FastModeDef};

/// Catalog lifecycle status that disqualifies a fast-mode tier from use:
/// the provider has announced its removal and `param=value` either errors
/// or silently degrades to standard serving.
const DEPRECATED_STATUS: &str = "deprecated";

/// Resolve the model's accelerated-serving tier from the catalog, if any.
pub(crate) fn lookup(model: &str) -> Option<FastModeDef> {
    model_catalog_entry(model).and_then(|entry| entry.fast_mode)
}

/// Whether a fast-mode tier is currently usable. A `deprecated` tier is
/// still described in the catalog (so callers can migrate) but must not be
/// engaged on new requests.
pub(crate) fn is_usable(fast_mode: &FastModeDef) -> bool {
    fast_mode.status.as_deref() != Some(DEPRECATED_STATUS)
}

/// Outcome of validating a `fast: true` request against the catalog.
pub(crate) enum FastModeGate {
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
pub(crate) fn gate(model: &str) -> FastModeGate {
    match lookup(model) {
        None => FastModeGate::Unsupported,
        Some(fast_mode) if !is_usable(&fast_mode) => FastModeGate::Deprecated {
            note: fast_mode.note,
        },
        Some(_) => FastModeGate::Usable,
    }
}

/// Inject the fast-mode request knob into an already-built provider body.
/// No-op when `fast` is false or the model has no usable fast-mode tier, so
/// it is safe to call unconditionally from every provider body builder.
pub(crate) fn apply_request_knob(body: &mut serde_json::Value, model: &str, fast: bool) {
    if !fast {
        return;
    }
    let Some(fast_mode) = lookup(model).filter(is_usable) else {
        return;
    };
    if let Some(object) = body.as_object_mut() {
        object.insert(fast_mode.param, serde_json::Value::String(fast_mode.value));
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
    lookup(model)
        .filter(is_usable)
        .and_then(|fast_mode| fast_mode.beta_header)
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
    let Some(fast_mode) = lookup(model) else {
        return false;
    };
    let matches = |scope: &serde_json::Value| {
        scope.get(&fast_mode.param).and_then(|v| v.as_str()) == Some(fast_mode.value.as_str())
    };
    matches(obj) || obj.get("usage").map(matches).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_resolves_anthropic_speed_knob() {
        let fast = lookup("claude-opus-4-8").expect("opus 4.8 advertises fast mode");
        assert_eq!(fast.param, "speed");
        assert_eq!(fast.value, "fast");
        assert_eq!(fast.beta_header.as_deref(), Some("fast-mode-2026-02-01"));
        assert!(is_usable(&fast));
    }

    #[test]
    fn gate_rejects_unsupported_and_deprecated() {
        assert!(matches!(gate("gpt-4o"), FastModeGate::Unsupported));
        // Opus 4.6's fast tier has been removed from the catalog after
        // Anthropic's June 29, 2026 removal date.
        assert!(matches!(gate("claude-opus-4-6"), FastModeGate::Unsupported));

        let overlay = crate::llm_config::parse_config_toml(
            r#"
[models."test-deprecated-fast"]
name = "Deprecated Fast Test"
provider = "test"
context_window = 128000
fast_mode = { param = "speed", value = "fast", status = "deprecated", note = "removed" }
"#,
        )
        .expect("test catalog overlay");
        crate::llm_config::set_user_overrides(Some(overlay));
        assert!(matches!(
            gate("test-deprecated-fast"),
            FastModeGate::Deprecated { .. }
        ));
        crate::llm_config::clear_user_overrides();

        assert!(matches!(gate("gpt-5.5"), FastModeGate::Usable));
    }

    #[test]
    fn apply_request_knob_sets_provider_field() {
        let mut anthropic = serde_json::json!({"model": "claude-opus-4-8"});
        apply_request_knob(&mut anthropic, "claude-opus-4-8", true);
        assert_eq!(anthropic["speed"], serde_json::json!("fast"));

        let mut openai = serde_json::json!({"model": "gpt-5.5"});
        apply_request_knob(&mut openai, "gpt-5.5", true);
        assert_eq!(openai["service_tier"], serde_json::json!("fast"));
    }

    #[test]
    fn apply_request_knob_is_noop_when_off_or_unsupported() {
        let mut body = serde_json::json!({"model": "claude-opus-4-8"});
        apply_request_knob(&mut body, "claude-opus-4-8", false);
        assert!(body.get("speed").is_none());

        let mut unsupported = serde_json::json!({"model": "gpt-4o"});
        apply_request_knob(&mut unsupported, "gpt-4o", true);
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
