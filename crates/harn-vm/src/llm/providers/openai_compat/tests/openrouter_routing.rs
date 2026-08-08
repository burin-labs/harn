//! OpenRouter upstream-routing directives on the assembled body.

use super::fixtures::base_request_payload;
use crate::llm::providers::openai_compat::{
    apply_openrouter_provider_order, apply_openrouter_route_denylist,
    ensure_openrouter_require_parameters, OpenAiCompatibleProvider,
};
use serde_json::json;

#[test]
fn openrouter_structured_output_requires_supported_parameters() {
    let mut payload = base_request_payload();
    payload.output_format = crate::llm::api::OutputFormat::JsonSchema {
        schema: serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"],
        }),
        strict: true,
    };

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["provider"]["require_parameters"], true);
}

#[test]
fn openrouter_require_parameters_preserves_provider_preferences() {
    let mut body = serde_json::json!({
        "model": "google/gemma-4-26b-a4b-it",
        "messages": [],
        "response_format": {"type": "json_schema"},
        "provider": {"order": ["Fireworks"], "sort": "throughput"},
    });

    ensure_openrouter_require_parameters(&mut body);

    assert_eq!(body["provider"]["order"][0], "Fireworks");
    assert_eq!(body["provider"]["sort"], "throughput");
    assert_eq!(body["provider"]["require_parameters"], true);
}

#[test]
fn openrouter_emits_top_k_only_when_capability_allows() {
    let mut payload = base_request_payload();
    payload.model = "google/gemma-4-26b-a4b-it".to_string();
    payload.top_k = Some(64);
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert_eq!(body["top_k"].as_i64(), Some(64));
    assert_eq!(body["provider"]["require_parameters"], true);

    payload.model = "mistralai/devstral-small".to_string();
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert!(body.get("top_k").is_none());
}

#[test]
fn route_denylist_seeds_provider_ignore_on_empty_body() {
    let mut body = json!({"model": "qwen/qwen3.6-35b-a3b"});
    apply_openrouter_route_denylist(&mut body, &["Ambient".to_string()]);
    assert_eq!(body["provider"]["ignore"], json!(["Ambient"]));
}

#[test]
fn route_denylist_merges_and_dedupes_existing_ignore() {
    let mut body = json!({
        "model": "qwen/qwen3.6-35b-a3b",
        "provider": { "ignore": ["X"], "require_parameters": true }
    });
    apply_openrouter_route_denylist(&mut body, &["Ambient".to_string(), "X".to_string()]);
    // Existing entry preserved, new entry appended, duplicate not re-added.
    assert_eq!(body["provider"]["ignore"], json!(["X", "Ambient"]));
    // Unrelated provider keys are left untouched.
    assert_eq!(body["provider"]["require_parameters"], json!(true));
}

#[test]
fn route_denylist_noop_for_empty_deny() {
    let mut body = json!({"model": "qwen/qwen3.6-35b-a3b"});
    apply_openrouter_route_denylist(&mut body, &[]);
    assert!(body.get("provider").is_none());
}

#[test]
fn build_request_body_applies_qwen36_ambient_denylist_for_openrouter_only() {
    // The qwen3.6 openrouter capability row carries
    // provider_route_denylist = ["Ambient"]; build_request_body must
    // materialize it into provider.ignore for the openrouter provider.
    let mut payload = base_request_payload();
    payload.provider = "openrouter".to_string();
    payload.model = "qwen/qwen3.6-35b-a3b".to_string();
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    let ignore = body["provider"]["ignore"]
        .as_array()
        .expect("provider.ignore array present for qwen3.6 openrouter route");
    assert!(
        ignore.iter().any(|v| v.as_str() == Some("Ambient")),
        "qwen3.6 openrouter body must deny the Ambient upstream: {body}"
    );

    // A non-openrouter provider serving the same model id must NOT get a
    // provider.ignore block — the denylist is openrouter-scoped.
    let mut other = base_request_payload();
    other.provider = "vllm".to_string();
    other.model = "qwen/qwen3.6-35b-a3b".to_string();
    let other_body = OpenAiCompatibleProvider::build_request_body(&other, false);
    assert!(
        other_body.get("provider").is_none(),
        "non-openrouter provider must not receive provider.ignore: {other_body}"
    );
}

#[test]
fn provider_order_pins_closed_allowlist() {
    let mut body = json!({"model": "openai/gpt-oss-120b"});
    apply_openrouter_provider_order(&mut body, &["Cerebras".to_string(), "Groq".to_string()]);
    assert_eq!(body["provider"]["order"], json!(["Cerebras", "Groq"]));
    assert_eq!(body["provider"]["allow_fallbacks"], json!(false));
}

#[test]
fn provider_order_respects_caller_order_but_forces_closed() {
    // A caller-supplied order is preserved; allow_fallbacks is still forced
    // false so the pin is genuinely closed.
    let mut body = json!({
        "model": "openai/gpt-oss-120b",
        "provider": { "order": ["Groq"], "allow_fallbacks": true }
    });
    apply_openrouter_provider_order(&mut body, &["Cerebras".to_string(), "Groq".to_string()]);
    assert_eq!(body["provider"]["order"], json!(["Groq"]));
    assert_eq!(body["provider"]["allow_fallbacks"], json!(false));
}

#[test]
fn provider_order_noop_for_empty() {
    let mut body = json!({"model": "openai/gpt-oss-120b"});
    apply_openrouter_provider_order(&mut body, &[]);
    assert!(body.get("provider").is_none());
}

#[test]
fn build_request_body_pins_gpt_oss_openrouter_to_clean_subproviders() {
    // The openrouter openai/gpt-oss-* capability row carries
    // openrouter_provider_order = ["Cerebras", "Groq"]; build_request_body
    // must materialize it into provider.order + allow_fallbacks:false so the
    // sub-provider lottery only lands on known-clean upstreams.
    let mut payload = base_request_payload();
    payload.provider = "openrouter".to_string();
    payload.model = "openai/gpt-oss-120b".to_string();
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert_eq!(
        body["provider"]["order"],
        json!(["Cerebras", "Groq"]),
        "gpt-oss openrouter body must pin the clean upstream order: {body}"
    );
    assert_eq!(
        body["provider"]["allow_fallbacks"],
        json!(false),
        "gpt-oss openrouter pin must be closed (no fallbacks): {body}"
    );
}

#[test]
fn build_request_body_does_not_pin_other_openrouter_models() {
    // A non-gpt-oss openrouter model must not receive a provider.order pin —
    // the allowlist is row-scoped, so unrelated routes keep free routing.
    let mut payload = base_request_payload();
    payload.provider = "openrouter".to_string();
    payload.model = "anthropic/claude-sonnet-4.5".to_string();
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    let order = body
        .get("provider")
        .and_then(|provider| provider.get("order"));
    assert!(
        order.is_none(),
        "non-gpt-oss openrouter route must not be pinned: {body}"
    );
}

#[test]
fn build_request_body_does_not_pin_gpt_oss_on_other_providers() {
    // gpt-oss served by a NON-openrouter provider (groq/cerebras direct)
    // must NOT get a provider.order block — the pin is openrouter-scoped.
    let mut payload = base_request_payload();
    payload.provider = "groq".to_string();
    payload.model = "openai/gpt-oss-120b".to_string();
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert!(
        body.get("provider").is_none(),
        "non-openrouter gpt-oss route must not be pinned: {body}"
    );
}
