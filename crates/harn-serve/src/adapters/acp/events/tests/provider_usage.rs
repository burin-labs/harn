use super::*;

pub(super) async fn provider_usage_notification() -> serde_json::Value {
    let usage = serde_json::from_value(serde_json::json!({
        "run_id": "run-1", "parent_session_id": "", "lineage_known": true,
        "input_tokens": 3, "output_tokens": 1, "cost_usd": null,
        "known_cost_usd": 0.002, "provider_call_count": 2,
        "unpriced_calls": 1, "usage_unknown_calls": 1,
        "cache_read_tokens": 0, "cache_write_tokens": 0
    }))
    .expect("usage fixture matches the runtime ledger");
    let event = AgentEvent::ProviderUsage {
        session_id: "session-1".into(),
        call_id: "call-1".into(),
        provider: "openai".into(),
        model: "model-1".into(),
        run_usage: Box::new(usage),
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&event);
    let line = rx.recv().await.expect("provider usage notification");
    serde_json::from_str(&line).expect("notification JSON")
}

#[tokio::test(flavor = "current_thread")]
async fn provider_usage_projects_the_physical_ledger_on_the_extension_stream() {
    let notification = provider_usage_notification().await;
    assert_eq!(notification["method"], HARN_AGENT_EVENT_METHOD);
    assert_eq!(notification["params"]["kind"], "provider_usage");
    assert_eq!(notification["params"]["call_id"], "call-1");
    let usage = &notification["params"]["run_usage"];
    assert_eq!(usage["provider_call_count"], 2);
    assert_eq!(usage["known_cost_usd"], 0.002);
    assert!(usage["cost_usd"].is_null());
    assert_eq!(usage["unpriced_calls"], 1);
    assert_eq!(usage["usage_unknown_calls"], 1);
}
