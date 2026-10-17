//! Pure routing projection cases moved with their implementation.

use super::openrouter::*;
use serde_json::json;

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
fn provider_order_pins_closed_allowlist() {
    let mut body = json!({"model": "openai/gpt-oss-120b"});
    apply_openrouter_provider_order(&mut body, &["Cerebras".to_string(), "Groq".to_string()]);
    assert_eq!(body["provider"]["order"], json!(["Cerebras", "Groq"]));
    assert_eq!(body["provider"]["only"], json!(["Cerebras", "Groq"]));
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
    assert_eq!(body["provider"]["only"], json!(["Groq"]));
    assert_eq!(body["provider"]["allow_fallbacks"], json!(false));
}

#[test]
fn provider_order_noop_for_empty() {
    let mut body = json!({"model": "openai/gpt-oss-120b"});
    apply_openrouter_provider_order(&mut body, &[]);
    assert!(body.get("provider").is_none());
}

#[test]
fn provider_order_preserves_a_narrower_caller_allowlist() {
    let mut body = json!({
        "model": "openai/gpt-oss-120b",
        "provider": {"only": ["Groq"]}
    });
    apply_openrouter_provider_order(&mut body, &["Cerebras".to_string(), "Groq".to_string()]);
    assert_eq!(body["provider"]["order"], json!(["Cerebras", "Groq"]));
    assert_eq!(body["provider"]["only"], json!(["Groq"]));
    assert_eq!(body["provider"]["allow_fallbacks"], false);
}
