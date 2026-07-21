use super::*;

#[tokio::test]
async fn connector_worker_inherits_pipeline_egress_policy() {
    let env = crate::egress::test_env_guard();
    env.set(crate::egress::HARN_EGRESS_DEFAULT_ENV, "deny");
    let _scope = crate::egress::scope_egress_policy_for_current_thread();
    assert!(
        crate::egress::client_error_for_url("seed", "https://example.invalid").is_some(),
        "environment policy should be installed before the worker starts"
    );
    let (_dir, module_path) = write_connector(
        r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
pub fn call(_method, _args) {
  http_get("https://example.invalid")
  return {}
}
"#,
    );
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let mut effects = HarnConnectorEffectPolicies::default();
    effects.trust_export("call");
    let mut connector = HarnConnector::load_with_effect_policies(&module_path, effects)
        .await
        .unwrap();
    connector.init(ctx(log).await).await.unwrap();

    let error = connector
        .client()
        .call("probe", json!({}))
        .await
        .expect_err("pipeline egress policy should block connector worker HTTP");
    connector.shutdown(StdDuration::ZERO).await.unwrap();

    let message = error.to_string();
    assert!(message.contains("\"type\":\"EgressBlocked\""), "{message}");
    assert!(
        message.contains("\"host\":\"example.invalid\""),
        "{message}"
    );
}
