//! Prompt-cache breakpoint placement.

use super::fixtures::{base_request_payload, cache_control_count};
use crate::llm::providers::openai_compat::OpenAiCompatibleProvider;
use serde_json::json;

#[test]
fn openrouter_anthropic_cache_uses_top_level_breakpoint() {
    let mut payload = base_request_payload();
    payload.model = "anthropic/claude-sonnet-4-6".to_string();
    payload.cache = true;

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(cache_control_count(&body), 1);
}

#[test]
fn openrouter_qwen_explicit_cache_uses_last_content_block() {
    let mut payload = base_request_payload();
    payload.model = "qwen/qwen3.6-plus".to_string();
    payload.cache = true;
    payload.messages = vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "stable reference"},
            {"type": "text", "text": "question"}
        ],
    })];

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert!(body.get("cache_control").is_none());
    assert_eq!(
        body["messages"][0]["content"][1]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(cache_control_count(&body), 1);
}

#[test]
fn openrouter_gemini_explicit_cache_uses_last_content_block() {
    let mut payload = base_request_payload();
    payload.model = "google/gemini-2.5-flash".to_string();
    payload.cache = true;
    payload.messages = vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "stable reference"},
            {"type": "text", "text": "question"}
        ],
    })];

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(
        body["messages"][0]["content"][1]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(cache_control_count(&body), 1);
}

#[test]
fn openrouter_automatic_cache_route_does_not_emit_cache_control() {
    let mut payload = base_request_payload();
    payload.model = "deepseek/deepseek-v3".to_string();
    payload.cache = true;

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(cache_control_count(&body), 0);
}

#[test]
fn openrouter_qwen_open_weight_route_does_not_emit_cache_control() {
    let mut payload = base_request_payload();
    payload.model = "qwen/qwen3.6-35b-a3b".to_string();
    payload.cache = true;

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(cache_control_count(&body), 0);
}

#[test]
fn openrouter_explicit_cache_preserves_existing_message_breakpoint() {
    let mut payload = base_request_payload();
    payload.model = "qwen/qwen3-coder-plus".to_string();
    payload.cache = true;
    payload.messages = vec![json!({
        "role": "user",
        "content": [
            {
                "type": "text",
                "text": "stable reference",
                "cache_control": {"type": "ephemeral"}
            },
            {"type": "text", "text": "question"}
        ],
    })];

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert!(body["messages"][0]["content"][1]
        .get("cache_control")
        .is_none());
    assert_eq!(cache_control_count(&body), 1);
}

#[test]
fn openrouter_cache_preserves_existing_tool_breakpoint() {
    let mut payload = base_request_payload();
    payload.model = "anthropic/claude-sonnet-4-6".to_string();
    payload.cache = true;
    payload.native_tools = Some(vec![json!({
        "type": "function",
        "cache_control": {"type": "ephemeral"},
        "function": {
            "name": "lookup",
            "description": "Lookup stable context",
            "parameters": {"type": "object"}
        }
    })]);

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert!(body.get("cache_control").is_none());
    assert_eq!(
        body["tools"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(cache_control_count(&body), 1);
}
