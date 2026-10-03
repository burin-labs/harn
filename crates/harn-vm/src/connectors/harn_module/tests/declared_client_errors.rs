use super::*;

#[tokio::test]
async fn authored_connector_errors_keep_their_client_classification() {
    let (_dir, module_path) = write_connector(
        r#"
pub fn provider_id() { return "webhook" }
pub fn kinds() { return ["webhook"] }
pub fn payload_schema() { return "GenericWebhookPayload" }
pub fn call(harness: Harness, method, args) {
  if method == "missing" { throw "method_not_found: absent" }
  if method == "invalid" { throw "invalid_args: payload" }
  if method == "limited" { throw "rate_limited: retry" }
  if method == "other" { throw {message: "detail", code: 7} }
  return {ok: true}
}
"#,
    );
    let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(32)));
    let mut connector = HarnConnector::load(&module_path).await.unwrap();
    connector.init(ctx(log).await).await.unwrap();
    let client = connector.client();
    assert!(matches!(client.call("missing", json!({})).await,
        Err(ClientError::MethodNotFound(detail)) if detail == "absent"));
    assert!(matches!(client.call("invalid", json!({})).await,
        Err(ClientError::InvalidArgs(detail)) if detail == "payload"));
    assert!(matches!(client.call("limited", json!({})).await,
        Err(ClientError::RateLimited(detail)) if detail == "retry"));
    let Err(ClientError::Other(detail)) = client.call("other", json!({})).await else {
        panic!("structured authored error must retain its payload");
    };
    assert_eq!(
        serde_json::from_str::<JsonValue>(&detail).unwrap(),
        json!({"message": "detail", "code": 7})
    );
    assert_eq!(
        client.call("productive", json!({})).await.unwrap(),
        json!({"ok": true})
    );
    connector.shutdown(StdDuration::ZERO).await.unwrap();
}
