use super::*;

#[tokio::test]
async fn send_message_preserves_declared_application_error_as_typed_task_metadata() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
type TriageError = {variant: "NotFound", message: string}

pub fn triage(task: string) -> string throws TriageError {
  let _ = task
  throw {variant: "NotFound", message: "PRIVATE-CUSTOMER-DIAGNOSTIC-123456"}
}
"#,
    )
    .expect("write script");
    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
    let request = harn_vm::jsonrpc::request(
        "typed-error-1",
        "message/send",
        json!({
            "message": {
                "metadata": {"target_agent": "triage"},
                "parts": [{"type": "text", "text": "missing widget"}]
            }
        }),
    );

    let processed = server.process_rpc(request, AuthRequest::default()).await;
    let RpcOutcome::Json(response) = processed.outcome else {
        panic!("expected json response");
    };

    assert_eq!(response["result"]["status"]["state"], "failed");
    assert_eq!(
        response["result"]["metadata"]["harn"]["applicationError"],
        json!({
            "tool": "triage",
            "data": {
                "variant": "NotFound",
                "message": "PRIVATE-CUSTOMER-DIAGNOSTIC-123456",
            },
        })
    );
    let human_text = response["result"]["history"][1]["parts"][0]["text"]
        .as_str()
        .expect("human error text");
    assert!(human_text.contains("declared application error"));
    assert!(!human_text.contains("PRIVATE-CUSTOMER-DIAGNOSTIC"));
}
