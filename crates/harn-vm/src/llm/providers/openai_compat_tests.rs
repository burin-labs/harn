use super::openai_compat::OpenAiCompatibleProvider;
use crate::llm::api::{options::base_opts, LlmRequestPayload, ReasoningEffort, ThinkingConfig};
use serde_json::json;

#[test]
fn forced_native_tool_search_keeps_extensions_for_unknown_proxy_model() {
    let mut opts = base_opts("openrouter");
    opts.model = "my-custom/gpt-forward".to_string();
    opts.provider_overrides = Some(serde_json::json!({"force_native_tool_search": true}));
    opts.native_tools = Some(vec![
        serde_json::json!({
            "type": "tool_search",
            "mode": "hosted",
            "namespaces": ["ops"],
        }),
        serde_json::json!({
            "type": "function",
            "namespace": "ops",
            "defer_loading": true,
            "function": {
                "name": "deploy",
                "description": "Deploy the app",
                "parameters": {"type": "object"},
            },
        }),
    ]);
    let payload = LlmRequestPayload::from(&opts);

    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["tools"][0]["type"], "tool_search");
    assert_eq!(body["tools"][1]["namespace"], "ops");
    assert_eq!(body["tools"][1]["defer_loading"], true);
}

#[test]
fn build_request_body_strips_storage_only_message_fields() {
    // The durable transcript carries storage-only fields on prior turns.
    // Strict OpenAI-compat providers reject unknown top-level message fields
    // with terminal HTTP 400s, so the request boundary retains only portable
    // OpenAI message keys.
    let mut opts = base_opts("openai");
    opts.model = "gpt-4.1".to_string();
    opts.messages = vec![
        json!({
            "role": "user",
            "content": "hello",
            "private_reasoning": "storage only",
            "cache_control": {"type": "ephemeral"},
            "reasoning_content": "wrong role",
            "tool_calls": [{
                "id": "wrong_role",
                "type": "function",
                "function": {"name": "read", "arguments": "{}"},
            }],
        }),
        json!({
            "role": "assistant",
            "content": [
                {"type": "reasoning", "text": "hidden chain", "visibility": "private"},
                {"type": "output_text", "text": "visible answer", "visibility": "public"}
            ],
            "reasoning": "let me inspect the file before editing",
            "private_reasoning": "storage only",
            "thinking": {"signature": "provider-private"},
            "cache_control": {"type": "ephemeral"},
            "tool_call_id": "wrong_role",
            "reasoning_content": "provider-private trace",
            "tool_calls": [{
                "id": "call_001",
                "type": "function",
                "function": {
                    "name": "read",
                    "arguments": "{\"path\":\"main.rs\"}",
                    "approxNumTokens": 0,
                },
                "name": "wrong_top_level_name",
                "arguments": {"ignored": true},
                "approxNumTokens": 0,
                "is_risky": "false",
                "index": 0,
            }, {
                "id": "call_002",
                "type": "burin-internal",
                "name": "write",
                "arguments": {"path": "main.rs"},
                "approxNumTokens": 0,
            }],
        }),
        json!({
            "role": "tool",
            "tool_call_id": "call_001",
            "content": "{\"ok\":true}",
            "tool_calls": [{
                "id": "wrong_role",
                "type": "function",
                "function": {"name": "read", "arguments": "{}"},
            }],
            "reasoning": "storage only",
            "reasoning_content": "wrong role",
        }),
    ];
    let payload = LlmRequestPayload::from(&opts);
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);
    let messages = body["messages"].as_array().expect("messages array");
    for message in messages {
        for key in [
            "reasoning",
            "private_reasoning",
            "thinking",
            "cache_control",
            "reasoning_content",
        ] {
            assert!(
                message.get(key).is_none(),
                "outgoing message must not carry `{key}`: {message}"
            );
        }
    }
    let user = &messages[0];
    assert_eq!(user["role"], "user");
    assert!(user.get("tool_calls").is_none());
    assert!(user.get("reasoning_content").is_none());

    let assistant = &messages[1];
    assert_eq!(assistant["role"], "assistant");
    assert!(assistant.get("tool_call_id").is_none());
    assert_eq!(assistant["content"][0]["text"], "visible answer");
    assert!(
        !assistant["content"].to_string().contains("hidden chain"),
        "private reasoning content block rode into OpenAI-compatible request: {assistant}"
    );
    let first_call = &assistant["tool_calls"][0];
    assert_eq!(first_call["id"], "call_001");
    assert_eq!(first_call["type"], "function");
    assert_eq!(first_call["function"]["name"], "read");
    assert_eq!(
        first_call["function"]["arguments"],
        "{\"path\":\"main.rs\"}"
    );
    for key in ["approxNumTokens", "is_risky", "index", "name", "arguments"] {
        assert!(
            first_call.get(key).is_none(),
            "outgoing tool_call must not carry `{key}`: {first_call}"
        );
    }
    assert!(
        first_call["function"].get("approxNumTokens").is_none(),
        "outgoing tool_call function must not carry provider-private fields: {first_call}"
    );

    let second_call = &assistant["tool_calls"][1];
    assert_eq!(second_call["id"], "call_002");
    assert_eq!(second_call["type"], "function");
    assert_eq!(second_call["function"]["name"], "write");
    assert_eq!(
        second_call["function"]["arguments"],
        "{\"path\":\"main.rs\"}"
    );
    assert!(
        second_call.get("approxNumTokens").is_none(),
        "flat Harn tool-call history must be normalized to OpenAI shape: {second_call}"
    );

    let tool = &messages[2];
    assert_eq!(tool["role"], "tool");
    assert_eq!(tool["tool_call_id"], "call_001");
    assert!(tool.get("tool_calls").is_none());
    assert!(tool.get("reasoning_content").is_none());
}

#[test]
fn moonshot_kimi_k3_replays_only_harn_owned_assistant_reasoning() {
    let mut opts = base_opts("moonshot");
    opts.model = "moonshot/kimi-k3".to_string();
    opts.messages = vec![
        json!({
            "role": "user",
            "content": "Read main.rs",
            "reasoning": "untrusted caller field",
        }),
        json!({
            "role": "assistant",
            "content": "",
            "reasoning": "Harn-owned reasoning",
            "reasoning_content": "untrusted seeded provider trace",
            "tool_calls": [{
                "id": "call_001",
                "type": "function",
                "function": {"name": "read", "arguments": "{\"path\":\"main.rs\"}"},
            }],
        }),
        json!({"role": "tool", "tool_call_id": "call_001", "content": "file contents"}),
    ];

    let moonshot_payload = LlmRequestPayload::from(&opts);
    let body = OpenAiCompatibleProvider::build_request_body(&moonshot_payload, false);
    let messages = body["messages"].as_array().expect("messages array");
    assert!(messages[0].get("reasoning_content").is_none());
    let assistant = &messages[1];
    assert_eq!(assistant["reasoning_content"], "Harn-owned reasoning");
    assert!(assistant.get("reasoning").is_none());
    assert_ne!(
        assistant["reasoning_content"],
        "untrusted seeded provider trace"
    );
    assert_eq!(messages[2]["tool_call_id"], "call_001");

    opts.provider = "openai".to_string();
    opts.model = "gpt-4.1".to_string();
    let generic_payload = LlmRequestPayload::from(&opts);
    let generic_body = OpenAiCompatibleProvider::build_request_body(&generic_payload, false);
    assert!(generic_body["messages"][1]
        .get("reasoning_content")
        .is_none());
}

#[test]
fn moonshot_kimi_k3_uses_only_its_catalog_authorized_request_knobs() {
    let mut opts = base_opts("moonshot");
    opts.model = "moonshot/kimi-k3".to_string();
    opts.max_tokens = 128;
    opts.temperature = Some(0.7);
    opts.top_p = Some(0.8);
    opts.frequency_penalty = Some(0.3);
    opts.presence_penalty = Some(0.2);
    opts.thinking = ThinkingConfig::Effort {
        level: ReasoningEffort::Max,
    };
    opts.native_tools = Some(vec![json!({
        "type": "function",
        "function": {
            "name": "read",
            "description": "Read a file.",
            "parameters": {"type": "object"},
        },
    })]);
    // A forced `required` is deliberately requested to prove the caller clamps
    // it: K3 always reasons, and Moonshot rejects forced tool_choice with HTTP
    // 400 "incompatible with thinking enabled", so `required` is not in K3's
    // `allowed_tool_choice_modes` (`["auto", "none"]`) and must degrade to auto.
    opts.tool_choice = Some(json!("required"));

    let payload = LlmRequestPayload::from(&opts);
    let body = OpenAiCompatibleProvider::build_request_body(&payload, false);

    assert_eq!(body["model"], "kimi-k3");
    assert_eq!(body["max_completion_tokens"], 128);
    assert_eq!(body["reasoning_effort"], "max");
    assert_eq!(body["tool_choice"], "auto");
    assert_eq!(body["tools"][0]["function"]["name"], "read");
    for field in [
        "max_tokens",
        "temperature",
        "top_p",
        "frequency_penalty",
        "presence_penalty",
    ] {
        assert!(
            body.get(field).is_none(),
            "K3 request leaked `{field}`: {body}"
        );
    }
}
