//! Claude generation gating: adaptive thinking, effort, and the sampling
//! parameters the 4.7+ surface rejects. Split out of `anthropic.rs` to keep
//! that module under the source-length ratchet.

use super::anthropic::{
    claude_generation, claude_model_supports_tool_search, model_defaults_to_adaptive_thinking,
    reconcile_request_body, strip_unsupported_sampling_params, AnthropicProvider,
};
use super::anthropic_test_support::base_payload;
use crate::llm::api::{ReasoningEffort, ThinkingConfig};

#[test]
fn fable_and_mythos_parse_generation_and_inherit_guards() {
    // Fable/Mythos 5 (launched 2026-06-09) share the Opus 4.7+ request
    // surface; the generation parser must recognize the families or none
    // of the >= (4, 6) / (4, 7) guards (prefill removal, sampling strip,
    // adaptive-thinking rewrite) fire for them.
    assert_eq!(claude_generation("claude-fable-5"), Some((5, 0)));
    assert_eq!(claude_generation("claude-mythos-5"), Some((5, 0)));
    assert_eq!(claude_generation("anthropic/claude-fable-5"), Some((5, 0)));
    assert_eq!(
        claude_generation("anthropic.claude-opus-4-7-v1:0"),
        Some((4, 7))
    );
    assert_eq!(
        claude_generation("anthropic.claude-3-5-sonnet-20240620-v1:0"),
        Some((3, 5))
    );
    // Mythos Preview has no numeric generation — stays unrecognized.
    assert_eq!(claude_generation("claude-mythos-preview"), None);
    assert!(claude_model_supports_tool_search("claude-fable-5"));
}

#[test]
fn fable_thinking_payloads_match_always_on_surface() {
    // Extended-thinking budgets are a 400 on Fable — rewritten to adaptive.
    let mut payload = base_payload();
    payload.model = "claude-fable-5".to_string();
    payload.thinking = ThinkingConfig::Enabled {
        budget_tokens: Some(4096),
    };
    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(body["thinking"], serde_json::json!({ "type": "adaptive" }));

    // Thinking is always on for Fable, and an explicit
    // `thinking: {type: "disabled"}` is also a 400 — a Disabled config
    // must leave the field out of the payload entirely.
    let mut payload2 = base_payload();
    payload2.model = "claude-fable-5".to_string();
    payload2.thinking = ThinkingConfig::Disabled;
    payload2.temperature = Some(0.0);
    let body2 = AnthropicProvider::build_request_body(&payload2);
    assert!(body2.get("thinking").is_none());
    // Sampling params are rejected on the 4.7+ surface — stripped.
    assert!(
        body2.get("temperature").is_none(),
        "temperature must be stripped for claude-fable-5"
    );
}

#[test]
fn opus_5_disabled_thinking_is_explicit_not_omitted() {
    // Through Opus 4.8, omitting `thinking` was the off switch. Opus 5
    // defaults it to adaptive, so an omitted field silently buys thinking
    // tokens the caller asked not to spend. Verified against the live API
    // on 2026-07-24: an omitted `thinking` returns thinking blocks.
    let mut payload = base_payload();
    payload.model = "claude-opus-5".to_string();
    payload.thinking = ThinkingConfig::Disabled;
    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(
        body["thinking"],
        serde_json::json!({ "type": "disabled" }),
        "Opus 5 thinks when `thinking` is omitted; Disabled must be explicit"
    );

    // Opus 4.8 keeps the omit-means-off surface.
    let mut prior = base_payload();
    prior.model = "claude-opus-4-8".to_string();
    prior.thinking = ThinkingConfig::Disabled;
    assert!(AnthropicProvider::build_request_body(&prior)
        .get("thinking")
        .is_none());
}

#[test]
fn opus_5_clamps_effort_when_thinking_is_disabled() {
    // `thinking:{disabled}` above effort `high` is a 400 on generation-5
    // models. The two halves are set independently — thinking by the
    // request builder, effort by a caller override merged in afterwards —
    // so the guard runs on the final body at the egress seam.
    let reconciled = |model: &str, effort: &str| {
        let mut body = serde_json::json!({
            "model": model,
            "thinking": {"type": "disabled"},
            "output_config": {"effort": effort},
        });
        reconcile_request_body(&mut body, model, &ThinkingConfig::Disabled);
        body
    };

    for requested in ["xhigh", "max"] {
        let body = reconciled("claude-opus-5", requested);
        assert_eq!(
            body["output_config"]["effort"],
            serde_json::json!("high"),
            "effort `{requested}` must clamp to `high` when thinking is disabled"
        );
        assert_eq!(body["thinking"], serde_json::json!({ "type": "disabled" }));
    }

    // `high` and below are legal on the same pairing.
    assert_eq!(
        reconciled("claude-opus-5", "medium")["output_config"]["effort"],
        serde_json::json!("medium")
    );

    // Thinking left on: `xhigh` is legal and must survive untouched.
    let mut thinking_on = serde_json::json!({
        "model": "claude-opus-5",
        "output_config": {"effort": "xhigh"},
    });
    reconcile_request_body(
        &mut thinking_on,
        "claude-opus-5",
        &ThinkingConfig::Effort {
            level: ReasoningEffort::XHigh,
        },
    );
    assert_eq!(
        thinking_on["output_config"]["effort"],
        serde_json::json!("xhigh"),
        "effort must not be clamped while thinking is active"
    );

    // Pre-generation-5 models accept the pair; nothing is clamped.
    assert_eq!(
        reconciled("claude-opus-4-8", "xhigh")["output_config"]["effort"],
        serde_json::json!("xhigh")
    );
}

#[test]
fn generation_5_drives_default_on_thinking_not_a_model_id_list() {
    // The rule is "generation >= 5", so a new gen-5 family inherits the
    // right surface instead of falling back to the 4.x omit-means-off
    // assumption.
    for model in [
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-fable-5",
        "claude-mythos-5",
        "anthropic/claude-opus-5",
    ] {
        assert!(
            model_defaults_to_adaptive_thinking(model),
            "{model} should default adaptive thinking on"
        );
    }
    for model in ["claude-opus-4-8", "claude-sonnet-4-6", "claude-haiku-4-5"] {
        assert!(
            !model_defaults_to_adaptive_thinking(model),
            "{model} needs an explicit thinking field to reason"
        );
    }
}

#[test]
fn sonnet_5_effort_uses_output_config_and_default_on_thinking() {
    let mut payload = base_payload();
    payload.model = "claude-sonnet-5".to_string();
    payload.thinking = ThinkingConfig::Effort {
        level: ReasoningEffort::High,
    };
    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(body["output_config"]["effort"], serde_json::json!("high"));
    assert!(
        body.get("thinking").is_none(),
        "Sonnet 5 defaults adaptive thinking on; effort should not send legacy thinking budgets"
    );

    let mut disabled = base_payload();
    disabled.model = "claude-sonnet-5".to_string();
    disabled.thinking = ThinkingConfig::Disabled;
    let disabled_body = AnthropicProvider::build_request_body(&disabled);
    assert_eq!(
        disabled_body["thinking"],
        serde_json::json!({ "type": "disabled" })
    );
    assert!(
        disabled_body.get("output_config").is_none(),
        "turning Sonnet 5 thinking off should not also send an effort level"
    );
}

#[test]
fn opus_adaptive_effort_uses_output_config_with_adaptive_thinking() {
    let mut payload = base_payload();
    payload.model = "claude-opus-4-7".to_string();
    payload.thinking = ThinkingConfig::Effort {
        level: ReasoningEffort::Max,
    };
    let body = AnthropicProvider::build_request_body(&payload);
    assert_eq!(body["thinking"], serde_json::json!({ "type": "adaptive" }));
    assert_eq!(body["output_config"]["effort"], serde_json::json!("max"));
}

#[test]
fn temperature_stripped_when_thinking_active() {
    // Anthropic rejects HTTP 400 if `temperature != 1` when thinking is
    // active. Strip the temperature transparently so callers can default
    // to temperature=0 for determinism without having to know which
    // models silently auto-enable thinking.
    let mut payload = base_payload();
    payload.temperature = Some(0.0);
    payload.thinking = ThinkingConfig::Adaptive;
    let body = AnthropicProvider::build_request_body(&payload);
    assert!(
        body.get("temperature").is_none(),
        "temperature must be stripped when thinking is active to avoid HTTP 400"
    );
    // Sanity: temperature is preserved when thinking is disabled.
    let mut payload2 = base_payload();
    payload2.temperature = Some(0.0);
    payload2.thinking = ThinkingConfig::Disabled;
    let body2 = AnthropicProvider::build_request_body(&payload2);
    assert_eq!(body2["temperature"], serde_json::json!(0.0));
}

#[test]
fn sampling_params_stripped_by_shared_helper_for_rejecting_models() {
    let mut body = serde_json::json!({
        "model": "claude-opus-4-7",
        "temperature": 0.2,
        "top_p": 0.9,
        "top_k": 20,
    });

    strip_unsupported_sampling_params(&mut body, "claude-opus-4-7", &ThinkingConfig::Disabled);

    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert!(body.get("top_k").is_none());
    assert_eq!(body["model"], serde_json::json!("claude-opus-4-7"));
}

#[test]
fn sampling_params_stripped_by_shared_helper_when_thinking_active() {
    let mut body = serde_json::json!({
        "model": "claude-sonnet-4-6",
        "temperature": 0.0,
        "top_p": 0.9,
        "top_k": 20,
    });

    strip_unsupported_sampling_params(&mut body, "claude-sonnet-4-6", &ThinkingConfig::Adaptive);

    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert!(body.get("top_k").is_none());
}

#[test]
fn sampling_params_preserved_by_shared_helper_for_supported_disabled_thinking() {
    let mut body = serde_json::json!({
        "model": "claude-sonnet-4-6",
        "temperature": 0.2,
        "top_p": 0.9,
        "top_k": 20,
    });

    strip_unsupported_sampling_params(&mut body, "claude-sonnet-4-6", &ThinkingConfig::Disabled);

    assert_eq!(body["temperature"], serde_json::json!(0.2));
    assert_eq!(body["top_p"], serde_json::json!(0.9));
    assert_eq!(body["top_k"], serde_json::json!(20));
}

#[test]
fn sampling_params_stripped_when_body_thinking_override_is_active() {
    let mut body = serde_json::json!({
        "model": "claude-sonnet-4-6",
        "temperature": 0.2,
        "top_p": 0.9,
        "thinking": {"type": "enabled", "budget_tokens": 1024},
    });

    strip_unsupported_sampling_params(&mut body, "claude-sonnet-4-6", &ThinkingConfig::Disabled);

    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert_eq!(
        body["thinking"],
        serde_json::json!({"type": "enabled", "budget_tokens": 1024})
    );
}

#[test]
fn sampling_params_stripped_when_body_output_config_effort_override_is_active() {
    let mut body = serde_json::json!({
        "model": "claude-sonnet-4-6",
        "temperature": 0.2,
        "top_p": 0.9,
        "output_config": {"effort": "high"},
    });

    strip_unsupported_sampling_params(&mut body, "claude-sonnet-4-6", &ThinkingConfig::Disabled);

    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert_eq!(body["output_config"], serde_json::json!({"effort": "high"}));
}
