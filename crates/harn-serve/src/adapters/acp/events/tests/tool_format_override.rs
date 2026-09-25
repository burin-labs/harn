use super::*;

#[tokio::test(flavor = "current_thread")]
async fn tool_format_override_agent_event_uses_camel_case_fields() {
    let actual = collect_notifications(vec![AgentEvent::ToolFormatOverride {
        session_id: "session-1".to_string(),
        provider: "openrouter".to_string(),
        model: "qwen/qwen3-coder".to_string(),
        requested_format: "native".to_string(),
        recommended_format: "text".to_string(),
        catalog_parity: "native_unreliable".to_string(),
        override_reason: Some("cross-check provider regression".to_string()),
        applied_format: Some("text".to_string()),
        steered: Some(true),
    }])
    .await;

    let notification = &actual[0];
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    let params = &notification["params"];
    assert_eq!(params["kind"], "tool_format_override");
    assert_eq!(params["sessionId"], "session-1");
    assert_eq!(params["provider"], "openrouter");
    assert_eq!(params["model"], "qwen/qwen3-coder");
    assert_eq!(params["requestedFormat"], "native");
    assert_eq!(params["recommendedFormat"], "text");
    assert_eq!(params["catalogParity"], "native_unreliable");
    assert_eq!(params["overrideReason"], "cross-check provider regression");
    assert_eq!(params["appliedFormat"], "text");
    assert_eq!(params["steered"], true);
}
