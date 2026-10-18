//! OpenRouter upstream-routing directives on the assembled body.

use super::fixtures::base_request_payload;
use crate::llm::providers::openai_compat::OpenAiCompatibleProvider;
use serde_json::json;

#[test]
fn option_probes_require_parameter_support_on_openrouter_only() {
    let mut payload = base_request_payload();
    payload.model = "openai/gpt-6-luna".to_string();
    payload.provider_contract_probe = Some(crate::llm::capabilities::PortableOption::Temperature);
    let body = OpenAiCompatibleProvider::build_request_body(&payload);
    assert_eq!(body["temperature"], 0.0);
    assert_eq!(body["provider"]["require_parameters"], true);

    payload.provider_contract_probe = None;
    let body = OpenAiCompatibleProvider::build_request_body(&payload);
    assert!(body.get("provider").is_none());

    payload.provider = "fireworks".to_string();
    payload.provider_contract_probe = Some(crate::llm::capabilities::PortableOption::Temperature);
    let body = OpenAiCompatibleProvider::build_request_body(&payload);
    assert!(body.get("provider").is_none());
}

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

    let body = OpenAiCompatibleProvider::build_request_body(&payload);

    assert_eq!(body["provider"]["require_parameters"], true);
}

#[test]
fn openrouter_builder_projects_top_k_after_admission() {
    let mut payload = base_request_payload();
    payload.model = "google/gemma-4-26b-a4b-it".to_string();
    payload.top_k = Some(64);
    let body = OpenAiCompatibleProvider::build_request_body(&payload);
    assert_eq!(body["top_k"].as_i64(), Some(64));
    assert_eq!(body["provider"]["require_parameters"], true);

    payload.model = "mistralai/devstral-small".to_string();
    let body = OpenAiCompatibleProvider::build_request_body(&payload);
    assert_eq!(body["top_k"].as_i64(), Some(64));
    assert_eq!(body["provider"]["require_parameters"], true);
}

#[test]
fn build_request_body_applies_qwen36_ambient_denylist_for_openrouter_only() {
    // The qwen3.6 openrouter capability row carries
    // provider_route_denylist = ["Ambient"]; build_request_body must
    // materialize it into provider.ignore for the openrouter provider.
    let mut payload = base_request_payload();
    payload.provider = "openrouter".to_string();
    payload.model = "qwen/qwen3.6-35b-a3b".to_string();
    let body = OpenAiCompatibleProvider::build_request_body(&payload);
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
    let other_body = OpenAiCompatibleProvider::build_request_body(&other);
    assert!(
        other_body.get("provider").is_none(),
        "non-openrouter provider must not receive provider.ignore: {other_body}"
    );
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
    let body = OpenAiCompatibleProvider::build_request_body(&payload);
    assert_eq!(body["provider"]["only"], json!(["Cerebras", "Groq"]));
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
    let body = OpenAiCompatibleProvider::build_request_body(&payload);
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
    let body = OpenAiCompatibleProvider::build_request_body(&payload);
    assert!(
        body.get("provider").is_none(),
        "non-openrouter gpt-oss route must not be pinned: {body}"
    );
}
