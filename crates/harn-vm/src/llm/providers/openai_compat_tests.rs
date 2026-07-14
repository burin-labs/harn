use super::openai_compat::OpenAiCompatibleProvider;
use crate::llm::api::{options::base_opts, LlmRequestPayload};

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
