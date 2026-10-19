use super::*;
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn private_session_input_control_reaches_the_real_pipeline_and_stays_revoked() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("inputs.harn");
    std::fs::write(&path, r#"
fn main(harness: Harness) {
  const before = harness.secrets.inputs()
  const removed = harness.secrets.remove_input("SERVICE_KEY")
  harness.stdio.println(json_stringify({reached: true, before: before, removed: removed, after: harness.secrets.inputs()}))
}
"#).unwrap();
    tokio::task::LocalSet::new().run_until(async {
        let (tx, mut rx, server, session) = start_acp_channel_session_with_config(
            AcpServerConfig::new(Some(path.to_string_lossy().into_owned())), json!(directory.path()),
        ).await;
        tx.send(json!({"jsonrpc":"2.0", "id":10, "method":"session/inputs", "params":{
            "sessionId":session, "command":{"kind":"put", "spec":{"name":"SERVICE_KEY"}, "value":"opaque-pipeline-canary"}
        }})).unwrap();
        assert!(recv_response_with_id(&mut rx, 10).await.get("result").is_some());
        for id in [11, 12] {
            tx.send(json!({"jsonrpc":"2.0", "id":id, "method":"session/prompt", "params":{
                "sessionId":session, "prompt":[{"type":"text", "text":"Inspect the private inputs."}]
            }})).unwrap();
            let mut output = String::new();
            let terminal = loop {
                let message = recv_json(&mut rx).await;
                assert!(!message.to_string().contains("opaque-pipeline-canary"));
                if message["method"] == "host/capabilities" {
                    tx.send(json!({"jsonrpc":"2.0", "id":message["id"], "result":{}})).unwrap();
                } else if message["params"]["update"]["sessionUpdate"] == "agent_message_chunk" {
                    output.push_str(message["params"]["update"]["content"]["text"].as_str().unwrap_or_default());
                } else if message["id"] == id { break message }
            };
            assert!(terminal.get("error").is_none(), "{terminal}");
            let receipt: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
            assert_eq!(receipt["reached"], true);
            assert_eq!(receipt["removed"], id == 11);
            assert_eq!(receipt["before"].as_array().unwrap().len(), usize::from(id == 11));
            assert!(receipt["after"].as_array().unwrap().is_empty());
            if id == 11 {
                assert_eq!(receipt["before"][0]["name"], "SERVICE_KEY");
                assert!(receipt["before"][0].get("value").is_none());
            }
        }
        drop(tx);
        server.await.unwrap();
    }).await;
}

#[tokio::test]
async fn private_input_control_returns_names_and_never_restores_values_after_close() {
    tokio::task::LocalSet::new().run_until(async {
        let (tx, mut rx, server, session) = start_acp_channel_session().await;
        tx.send(json!({"jsonrpc":"2.0", "id":10, "method":"session/inputs", "params":{
            "sessionId":session, "command":{"kind":"put", "spec":{"name":"SERVICE_KEY"}, "value":"opaque-session-canary"}
        }})).unwrap();
        let added = recv_response_with_id(&mut rx, 10).await;
        assert_eq!(added["result"]["input"]["name"], "SERVICE_KEY");
        assert!(!added.to_string().contains("opaque-session-canary"));
        tx.send(json!({"jsonrpc":"2.0", "id":11, "method":"session/inputs", "params":{
            "sessionId":session, "command":{"kind":"list"}
        }})).unwrap();
        let listed = recv_response_with_id(&mut rx, 11).await;
        assert_eq!(listed["result"]["inputs"].as_array().unwrap().len(), 1);
        assert!(!listed.to_string().contains("opaque-session-canary"));
        tx.send(json!({"jsonrpc":"2.0", "id":12, "method":"session/inputs", "params":{
            "sessionId":"not-this-session", "command":{"kind":"list"}
        }})).unwrap();
        assert_eq!(recv_response_with_id(&mut rx, 12).await["error"]["code"], -32004);
        tx.send(json!({"jsonrpc":"2.0", "id":13, "method":"session/close", "params":{"sessionId":session}})).unwrap();
        assert!(recv_response_with_id(&mut rx, 13).await.get("result").is_some());
        tx.send(json!({"jsonrpc":"2.0", "id":14, "method":"session/inputs", "params":{
            "sessionId":session, "command":{"kind":"list"}
        }})).unwrap();
        assert_eq!(recv_response_with_id(&mut rx, 14).await["error"]["code"], -32004);
        drop(tx);
        server.await.unwrap();
    }).await;
}

#[test]
fn active_input_control_uses_the_same_store_and_requires_authentication() {
    let controls = ConcurrentSessionControls::new(false, json!({}));
    let control = ConcurrentSessionControl::new();
    control.set_prompt_active(true);
    controls.register("active", control.clone());
    let captured = Arc::new(Mutex::new(Vec::<String>::new()));
    let lines = captured.clone();
    let output = AcpOutput::callback(move |line| lines.lock().unwrap().push(line.into()));
    let message = json!({"jsonrpc":"2.0", "id":1, "method":"session/inputs", "params":{
        "sessionId":"active", "command":{"kind":"put", "spec":{"name":"KEY"}, "value":"private-canary"}
    }});
    assert!(controls.preempt_active_session_inputs(&message, &output));
    assert!(control.inputs.lock().list(0).is_empty());
    controls.mark_authenticated();
    assert!(controls.preempt_active_session_inputs(&message, &output));
    assert_eq!(control.inputs.lock().list(0).len(), 1);
    assert_eq!(captured.lock().unwrap().len(), 2);
    assert!(captured
        .lock()
        .unwrap()
        .iter()
        .all(|line| !line.contains("private-canary")));
}
