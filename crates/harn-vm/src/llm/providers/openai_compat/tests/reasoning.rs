//! Reasoning/thinking request shapes across the OpenAI-compatible dialects.

use super::fixtures::base_request_payload;
use crate::llm::api::{ReasoningEffort, ThinkingConfig};
use crate::llm::provider::LlmProvider;
use crate::llm::providers::openai_compat::OpenAiCompatibleProvider;
use serde_json::json;

#[test]
fn openrouter_thinking_enabled_maps_to_reasoning_enabled() {
    let provider = OpenAiCompatibleProvider::new("openrouter".to_string());
    let mut payload = base_request_payload();
    payload.thinking = ThinkingConfig::Enabled {
        budget_tokens: None,
    };
    let mut body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    provider.transform_request(&mut body);

    assert_eq!(body["reasoning"]["enabled"], true);
    assert!(body.get("chat_template_kwargs").is_none());
}

#[test]
fn openrouter_no_reasoning_model_omits_reasoning_on_structured_disable() {
    // Regression: qwen/qwen3-coder declares no reasoning capability. With
    // a structured (json_object) call, `require_parameters: true` is set,
    // which makes OpenRouter exclude any endpoint lacking the `reasoning`
    // param. Emitting `reasoning: {enabled: false}` then drops every
    // candidate -> 404 "No endpoints found". The disable must be omitted.
    let mut payload = base_request_payload();
    payload.provider = "openrouter".to_string();
    payload.model = "qwen/qwen3-coder".to_string();
    payload.thinking = ThinkingConfig::Disabled;
    payload.output_format = crate::llm::api::OutputFormat::JsonObject;
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert!(
        body.get("reasoning").is_none(),
        "reasoning disable must be omitted for a no-reasoning model: {body}"
    );
    // The structured directive and require_parameters must still be present.
    assert_eq!(body["response_format"]["type"], "json_object");
    assert_eq!(body["provider"]["require_parameters"], true);
}

#[test]
fn openrouter_reasoning_capable_model_still_disables_on_directive() {
    // Control: a reasoning-capable OpenRouter model (qwen/qwen3.6* declares
    // thinking_modes + reasoning_none_supported) must keep the explicit
    // disable so it doesn't fall back to unbounded thinking.
    let mut payload = base_request_payload();
    payload.provider = "openrouter".to_string();
    payload.model = "qwen/qwen3.6-35b-a3b".to_string();
    payload.thinking = ThinkingConfig::Disabled;
    payload.output_format = crate::llm::api::OutputFormat::JsonObject;
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(
        body["reasoning"]["enabled"], false,
        "reasoning-capable model must keep the explicit disable: {body}"
    );
}

#[test]
fn openrouter_mandatory_reasoning_model_omits_unsupported_disable() {
    let mut payload = base_request_payload();
    payload.provider = "openrouter".to_string();
    payload.model = "stepfun/step-3.7-flash".to_string();
    payload.thinking = ThinkingConfig::Disabled;
    payload.output_format = crate::llm::api::OutputFormat::JsonObject;
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert!(
        body.get("reasoning").is_none(),
        "mandatory-reasoning route must omit unsupported disable: {body}"
    );
}

#[test]
fn openrouter_thinking_budget_maps_to_reasoning_max_tokens() {
    let provider = OpenAiCompatibleProvider::new("openrouter".to_string());
    let mut payload = base_request_payload();
    payload.thinking = ThinkingConfig::Enabled {
        budget_tokens: Some(2048),
    };
    let mut body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    provider.transform_request(&mut body);

    assert_eq!(body["reasoning"]["max_tokens"], 2048);
    assert!(body.get("chat_template_kwargs").is_none());
}

#[test]
fn openai_effort_maps_to_reasoning_effort() {
    let mut payload = base_request_payload();
    payload.provider = "openai".to_string();
    payload.model = "o3".to_string();
    payload.thinking = ThinkingConfig::Effort {
        level: ReasoningEffort::High,
    };
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["max_completion_tokens"], 64);
    assert!(body.get("max_tokens").is_none());
    assert!(body.get("reasoning").is_none());
}

#[test]
fn openai_none_effort_maps_to_reasoning_effort_none() {
    let mut payload = base_request_payload();
    payload.provider = "openai".to_string();
    payload.model = "gpt-5.5".to_string();
    payload.thinking = ThinkingConfig::Effort {
        level: ReasoningEffort::None,
    };
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["reasoning_effort"], "none");
}

#[test]
fn together_hybrid_reasoning_uses_reasoning_enabled() {
    let mut payload = base_request_payload();
    payload.provider = "together".to_string();
    payload.model = "moonshotai/Kimi-K2.5".to_string();
    payload.thinking = ThinkingConfig::Enabled {
        budget_tokens: None,
    };
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["reasoning"]["enabled"], true);
    assert!(body.get("chat_template_kwargs").is_none());
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn zai_glm52_effort_uses_thinking_object_and_reasoning_effort() {
    let mut payload = base_request_payload();
    payload.provider = "zai".to_string();
    payload.model = "glm-5.2".to_string();
    payload.thinking = ThinkingConfig::Effort {
        level: ReasoningEffort::Max,
    };

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["thinking"], json!({"type": "enabled"}));
    assert_eq!(body["reasoning_effort"], "max");
    assert!(
        body.get("reasoning").is_none(),
        "Z.AI uses `thinking`, not the generic `reasoning` object"
    );
    assert!(
        body.get("chat_template_kwargs").is_none(),
        "Z.AI GLM does not use Qwen-style chat_template_kwargs"
    );
}

#[test]
fn zai_glm52_disabled_uses_thinking_disabled() {
    let mut payload = base_request_payload();
    payload.provider = "zai".to_string();
    payload.model = "glm-5.2".to_string();
    payload.thinking = ThinkingConfig::Disabled;

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["thinking"], json!({"type": "disabled"}));
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("reasoning").is_none());
}

#[test]
fn zai_glm52_none_effort_is_explicitly_supported() {
    let mut payload = base_request_payload();
    payload.provider = "zai".to_string();
    payload.model = "glm-5.2".to_string();
    payload.thinking = ThinkingConfig::Effort {
        level: ReasoningEffort::None,
    };

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["thinking"], json!({"type": "disabled"}));
    assert_eq!(body["reasoning_effort"], "none");
    assert!(body.get("reasoning").is_none());
}

#[test]
fn minimax_m3_uses_adaptive_thinking_and_completion_tokens() {
    let mut payload = base_request_payload();
    payload.provider = "minimax".to_string();
    payload.model = "MiniMax-M3".to_string();
    payload.thinking = ThinkingConfig::Enabled {
        budget_tokens: Some(4096),
    };

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(body["reasoning_split"], true);
    assert_eq!(body["max_completion_tokens"], 64);
    assert!(body.get("max_tokens").is_none());
    assert!(body.get("reasoning").is_none());
}

#[test]
fn minimax_m3_disables_thinking_explicitly() {
    let mut payload = base_request_payload();
    payload.provider = "minimax".to_string();
    payload.model = "MiniMax-M3".to_string();
    payload.thinking = ThinkingConfig::Disabled;

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["thinking"]["type"], "disabled");
    assert!(body.get("reasoning_split").is_none());
}

#[test]
fn together_gpt_oss_effort_uses_reasoning_effort() {
    let mut payload = base_request_payload();
    payload.provider = "together".to_string();
    payload.model = "openai/gpt-oss-120b".to_string();
    payload.thinking = ThinkingConfig::Effort {
        level: ReasoningEffort::Medium,
    };
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["reasoning_effort"], "medium");
    assert!(body.get("reasoning").is_none());
}

#[test]
fn openrouter_effort_maps_to_nested_reasoning_effort() {
    let provider = OpenAiCompatibleProvider::new("openrouter".to_string());
    let mut payload = base_request_payload();
    payload.thinking = ThinkingConfig::Effort {
        level: ReasoningEffort::Medium,
    };
    let mut body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    provider.transform_request(&mut body);

    assert_eq!(body["reasoning"]["effort"], "medium");
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn openrouter_disabled_thinking_emits_reasoning_enabled_false() {
    // Qwen3 thinking variants honor explicit `{enabled: false}` but may
    // otherwise use their trained-default thinking budget.
    let provider = OpenAiCompatibleProvider::new("openrouter".to_string());
    let mut payload = base_request_payload();
    payload.thinking = ThinkingConfig::Disabled;
    let mut body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    provider.transform_request(&mut body);

    assert_eq!(body["reasoning"]["enabled"], false);
}
