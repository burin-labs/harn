use super::*;

#[tokio::test]
async fn dispatch_rejects_wrong_typed_pipeline_argument() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r"
pub pipeline deploy(replicas: int) -> bool {
  return replicas > 0
}
",
    )
    .expect("write script");

    let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let error = core
        .dispatch(CallRequest {
            adapter: "mcp".to_string(),
            function: "deploy".to_string(),
            arguments: CallArguments::Named(BTreeMap::from([(
                "replicas".to_string(),
                serde_json::json!("many"),
            )])),
            auth: AuthRequest::default(),
            caller: "tester".to_string(),
            replay_key: None,
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            agent_event_sink: None,
            actor_chain: None,
            actor_chain_hop: None,
            progress: None,
            tenant_id: None,
            request_id: None,
            auth_context: None,
            auth_principal: None,
        })
        .await
        .expect_err("typed pipeline rejects the wrong runtime argument");

    assert_eq!(
        error,
        DispatchError::Execution(
            "Runtime error: TypeError: parameter 'replicas' expected int, got string (many)"
                .to_string()
        )
    );
}
