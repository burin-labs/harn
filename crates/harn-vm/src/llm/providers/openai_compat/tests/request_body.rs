//! Request-body assembly: sampling knobs, token limits, serving tier, content
//! parts, structured-output directives, and chat-template options.

use super::fixtures::base_request_payload;
use crate::llm::api::ThinkingConfig;
use crate::llm::provider::LlmProvider;
use crate::llm::providers::openai_compat::OpenAiCompatibleProvider;
use serde_json::json;

#[test]
fn fast_tier_injects_service_tier_for_openai() {
    // `fast: true` on GPT-5.5 rides the catalog's `service_tier` knob;
    // OpenAI needs no beta header so none is added.
    let mut payload = base_request_payload();
    payload.provider = "openai".to_string();
    payload.model = "gpt-5.5".to_string();
    payload.fast = true;
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert_eq!(body["service_tier"], json!("fast"));

    payload.fast = false;
    let body_off = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert!(body_off.get("service_tier").is_none());
}

#[test]
fn build_request_body_clamps_sampling_ranges_before_send() {
    let mut payload = base_request_payload();
    payload.provider = "openai".to_string();
    payload.model = "gpt-4o".to_string();
    payload.temperature = Some(99.0);
    payload.top_p = Some(5000.0);

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["temperature"], json!(2.0));
    assert_eq!(body["top_p"], json!(1.0));

    payload.temperature = Some(f64::NEG_INFINITY);
    payload.top_p = Some(f64::NAN);
    let non_finite_body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert_eq!(non_finite_body["temperature"], json!(1.0));
    assert_eq!(non_finite_body["top_p"], json!(1.0));
}

#[test]
fn openrouter_kimi27_code_strips_fixed_sampling_params() {
    let mut payload = base_request_payload();
    payload.provider = "openrouter".to_string();
    payload.model = "moonshotai/kimi-k2.7-code".to_string();
    payload.temperature = Some(0.2);
    payload.top_p = Some(0.8);
    payload.frequency_penalty = Some(0.1);
    payload.presence_penalty = Some(0.2);

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert!(body.get("frequency_penalty").is_none());
    assert!(body.get("presence_penalty").is_none());
}

#[test]
fn grok_strips_stop_and_penalties_it_rejects() {
    // xAI returns HTTP 400 on `stop`, `frequency_penalty`, and
    // `presence_penalty` for every Grok model (live probe 2026-07-14). The
    // `grok-*` capability rule marks all three unsupported so the compat
    // layer drops them before dispatch. `temperature`/`top_p` are accepted
    // and must survive.
    let mut payload = base_request_payload();
    payload.provider = "xai".to_string();
    payload.model = "grok-4.5".to_string();
    payload.temperature = Some(0.7);
    payload.top_p = Some(0.9);
    payload.frequency_penalty = Some(0.1);
    payload.presence_penalty = Some(0.2);
    payload.stop = Some(vec!["STOP".to_string()]);

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert!(body.get("stop").is_none(), "grok rejects stop");
    assert!(
        body.get("frequency_penalty").is_none(),
        "grok rejects frequency_penalty"
    );
    assert!(
        body.get("presence_penalty").is_none(),
        "grok rejects presence_penalty"
    );
    assert!(
        body.get("temperature").is_some(),
        "grok accepts temperature"
    );
    assert!(body.get("top_p").is_some(), "grok accepts top_p");
}

#[test]
fn qwen36_emits_preserve_thinking_in_chat_template_kwargs() {
    let mut payload = base_request_payload();
    payload.provider = "local".to_string();
    payload.model = "Qwen/Qwen3.6-35B-A3B".to_string();
    payload.thinking = ThinkingConfig::Enabled {
        budget_tokens: None,
    };
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert_eq!(
        body["chat_template_kwargs"]["preserve_thinking"], true,
        "Qwen3.6 should request preserve_thinking so <think> blocks survive across agentic turns"
    );
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], true);
}

#[test]
fn build_request_body_uses_wire_model_for_catalog_key() {
    let mut payload = base_request_payload();
    payload.provider = "groq".to_string();
    payload.model = "groq/openai/gpt-oss-120b".to_string();

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["model"], "openai/gpt-oss-120b");
}

#[test]
fn transform_request_preserves_chat_template_kwargs_when_capability_allows() {
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.openrouter]]
model_match = "custom-qwen"
honors_chat_template_kwargs = true
thinking_modes = ["enabled"]
"#,
    )
    .expect("capability override");
    let provider = OpenAiCompatibleProvider::new("openrouter".to_string());
    let mut payload = base_request_payload();
    payload.model = "custom-qwen".to_string();
    payload.thinking = ThinkingConfig::Enabled {
        budget_tokens: None,
    };
    let mut body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert!(body.get("chat_template_kwargs").is_some());

    provider.transform_request(&mut body);

    assert!(body.get("chat_template_kwargs").is_some());
    crate::llm::capabilities::clear_user_overrides();
}

#[test]
fn build_request_body_uses_configured_chat_template_field() {
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.baseten]]
model_match = "zai-org/glm-5.2"
honors_chat_template_kwargs = true
chat_template_options_field = "chat_template_args"
thinking_modes = ["enabled"]
"#,
    )
    .expect("capability override");
    let provider = OpenAiCompatibleProvider::new("baseten".to_string());
    let mut payload = base_request_payload();
    payload.provider = "baseten".to_string();
    payload.model = "zai-org/GLM-5.2".to_string();
    payload.thinking = ThinkingConfig::Enabled {
        budget_tokens: None,
    };

    let mut body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert!(body.get("chat_template_args").is_some());
    assert!(body.get("chat_template_kwargs").is_none());
    body["chat_template_kwargs"] = json!({"enable_thinking": false});

    provider.transform_request(&mut body);

    assert!(body.get("chat_template_args").is_some());
    assert!(body.get("chat_template_kwargs").is_none());
    crate::llm::capabilities::clear_user_overrides();
}

#[test]
fn transform_request_strips_chat_template_kwargs_when_capability_denies() {
    let provider = OpenAiCompatibleProvider::new("acme".to_string());
    let mut body = json!({
        "model": "custom-qwen",
        "chat_template_kwargs": {"enable_thinking": true},
        "chat_template_args": {"enable_thinking": true},
    });

    provider.transform_request(&mut body);

    assert!(body.get("chat_template_kwargs").is_none());
    assert!(body.get("chat_template_args").is_none());
}

#[test]
fn ollama_qwen35_does_not_emit_chat_template_kwargs() {
    let mut payload = base_request_payload();
    payload.provider = "ollama".to_string();
    payload.model = "qwen3.5:35b-a3b-coding-nvfp4".to_string();
    payload.thinking = ThinkingConfig::Enabled {
        budget_tokens: None,
    };
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert!(
        body.get("chat_template_kwargs").is_none(),
        "Ollama silently drops chat_template_kwargs today; gate them so strict validation would not break requests"
    );
}

#[test]
fn qwen35_local_disables_thinking_when_absent() {
    let mut payload = base_request_payload();
    payload.provider = "local".to_string();
    payload.model = "Qwen/Qwen3.5-Coder-32B".to_string();
    payload.thinking = ThinkingConfig::Disabled;
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], false);
}

#[test]
fn openai_non_reasoning_model_uses_legacy_max_tokens() {
    let mut payload = base_request_payload();
    payload.provider = "openai".to_string();
    payload.model = "gpt-4o".to_string();

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["max_tokens"], 64);
    assert!(body.get("max_completion_tokens").is_none());
    assert!(body.get("reasoning_effort").is_none());
}

#[test]
fn image_content_maps_to_openai_image_url_block() {
    let mut payload = base_request_payload();
    payload.provider = "openai".to_string();
    payload.model = "gpt-4o".to_string();
    payload.messages = vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "caption"},
            {"type": "image", "base64": "iVBORw0KGgo=", "media_type": "image/png", "detail": "low"}
        ],
    })];

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert_eq!(body["messages"][0]["content"][0]["text"], "caption");
    assert_eq!(
        body["messages"][0]["content"][1],
        json!({
            "type": "image_url",
            "image_url": {
                "url": "data:image/png;base64,iVBORw0KGgo=",
                "detail": "low",
            }
        })
    );
}

#[test]
fn image_url_content_maps_to_openai_image_url_block() {
    let mut payload = base_request_payload();
    payload.provider = "openai".to_string();
    payload.model = "gpt-4o".to_string();
    payload.messages = vec![json!({
        "role": "user",
        "content": [
            {"type": "image", "url": "https://example.com/image.png", "media_type": "image/png", "detail": "high"}
        ],
    })];

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert_eq!(
        body["messages"][0]["content"][0],
        json!({
            "type": "image_url",
            "image_url": {
                "url": "https://example.com/image.png",
                "detail": "high",
            }
        })
    );
}

#[test]
fn video_content_maps_to_openai_video_url_block() {
    let mut payload = base_request_payload();
    payload.provider = "minimax".to_string();
    payload.model = "MiniMax-M3".to_string();
    payload.messages = vec![json!({
        "role": "user",
        "content": [
            {"type": "text", "text": "summarize"},
            {"type": "video", "base64": "AAAA", "media_type": "video/mp4"}
        ],
    })];

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    assert_eq!(body["messages"][0]["content"][0]["text"], "summarize");
    assert_eq!(
        body["messages"][0]["content"][1],
        json!({
            "type": "video_url",
            "video_url": {
                "url": "data:video/mp4;base64,AAAA",
            }
        })
    );
}

#[test]
fn output_format_json_schema_maps_to_openai_response_format() {
    let mut payload = base_request_payload();
    payload.output_format = crate::llm::api::OutputFormat::JsonSchema {
        schema: serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"],
        }),
        strict: false,
    };

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(
        body["response_format"]["json_schema"]["schema"]["properties"]["answer"]["type"],
        "string"
    );
    assert_eq!(
        body["response_format"]["json_schema"]["strict"],
        serde_json::json!(false)
    );
}

#[test]
fn cerebras_tools_drop_response_format() {
    let mut payload = base_request_payload();
    payload.provider = "cerebras".to_string();
    payload.model = "gpt-oss-120b".to_string();
    payload.output_format = crate::llm::api::OutputFormat::JsonObject;
    payload.native_tools = Some(vec![json!({
        "type": "function",
        "function": {
            "name": "read",
            "description": "Read a file",
            "parameters": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }
        }
    })]);

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert!(body.get("tools").is_some());
    assert!(
        body.get("response_format").is_none(),
        "Cerebras rejects tools + response_format together: {body}"
    );
}

#[test]
fn cerebras_keeps_response_format_without_tools() {
    let mut payload = base_request_payload();
    payload.provider = "cerebras".to_string();
    payload.model = "gpt-oss-120b".to_string();
    payload.output_format = crate::llm::api::OutputFormat::JsonObject;

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["response_format"]["type"], "json_object");
    assert!(body.get("tools").is_none());
}

#[test]
fn build_request_body_remaps_reserved_tool_call_token() {
    // llamacpp + qwen3.6 is flagged `reserved_tool_call_token` in
    // capabilities.toml, so the colliding delimiters must be remapped off
    // the wire across both system and history messages.
    let mut payload = base_request_payload();
    payload.provider = "llamacpp".to_string();
    payload.model = "qwen3.6-35b-a3b-ud-q4-k-xl".to_string();
    payload.system = Some("Use <tool_call>\nname({})\n</tool_call> blocks.".to_string());
    payload.messages = vec![json!({
        "role": "assistant",
        "content": "<tool_call>\nlook({})\n</tool_call>"
    })];
    let serialized = OpenAiCompatibleProvider::build_request_body(&payload, false).to_string();
    assert!(
        !serialized.contains("<tool_call>") && !serialized.contains("</tool_call>"),
        "canonical delimiters must be remapped off the wire: {serialized}"
    );
    assert!(
        serialized.contains("[[CALL]]") && serialized.contains("[[/CALL]]"),
        "non-special wire delimiters must be present: {serialized}"
    );
}

#[test]
fn build_request_body_keeps_canonical_for_normal_models() {
    // openrouter gemini is not a reserved-token model: leave the canonical
    // text tool-call delimiters exactly as authored.
    let mut payload = base_request_payload();
    payload.system = Some("Use <tool_call>\nname({})\n</tool_call> blocks.".to_string());
    let serialized = OpenAiCompatibleProvider::build_request_body(&payload, false).to_string();
    assert!(
        serialized.contains("<tool_call>"),
        "non-reserved model keeps canonical delimiter: {serialized}"
    );
    assert!(!serialized.contains("[[CALL]]"));
}
