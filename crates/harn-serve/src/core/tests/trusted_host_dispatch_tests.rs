use std::sync::Arc;

use super::*;

struct EchoHostBridge;

impl harn_vm::HostCallBridge for EchoHostBridge {
    fn dispatch<'a>(
        &'a self,
        capability: &'a str,
        operation: &'a str,
        params: &'a harn_vm::value::DictMap,
    ) -> harn_vm::stdlib::host::HostCallDispatchFuture<'a> {
        assert_eq!(capability, "cloud");
        assert_eq!(operation, "echo");
        harn_vm::host_call_ready(Ok(params.get("value").cloned()))
    }
}

fn request() -> CallRequest {
    CallRequest {
        adapter: "mcp".to_string(),
        function: "route".to_string(),
        arguments: CallArguments::Named(BTreeMap::from([(
            "value".to_string(),
            serde_json::json!("through-host"),
        )])),
        auth: AuthRequest::default(),
        caller: "trusted-host-dispatch-test".to_string(),
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
    }
}

#[tokio::test]
async fn dispatch_core_requires_explicit_trusted_host_authority() {
    let dir = tempfile::tempdir().expect("tempdir");
    let script = dir.path().join("server.harn");
    std::fs::write(
        &script,
        r#"
pub fn route(value: string) {
  return host_call("cloud.echo", {value: value})
}
"#,
    )
    .expect("write script");

    let ordinary = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
    let error = ordinary
        .dispatch(request())
        .await
        .expect_err("ordinary dispatch must reject host_call");
    assert!(error.message().contains("host_call"), "{error:?}");

    harn_vm::set_host_call_bridge(Arc::new(EchoHostBridge));
    let mut config = DispatchCoreConfig::for_script(&script);
    config.trusted_host_dispatch = true;
    let trusted = DispatchCore::new(config).expect("trusted core");
    let response = trusted.dispatch(request()).await.expect("trusted dispatch");
    harn_vm::clear_host_call_bridge();

    assert_eq!(response.value, serde_json::json!("through-host"));
}
