//! Exercise controls queued at the public channel transport's prompt boundary.

use harn_serve::{run_acp_channel_server, AcpServerConfig};
use serde_json::{json, Value};
use tokio::sync::mpsc;

async fn receive(rx: &mut mpsc::UnboundedReceiver<String>, label: &str) -> Value {
    let line = harn_clock::test_support::within(label, rx.recv())
        .await
        .expect("ACP output closed");
    serde_json::from_str(&line).expect("ACP JSON")
}

async fn response(rx: &mut mpsc::UnboundedReceiver<String>, id: u64) -> Value {
    loop {
        let message = receive(rx, &format!("ACP response {id}")).await;
        if message.get("id").is_some() && message.get("method").is_none() {
            assert_eq!(message["id"], id, "response overtook an earlier request");
            return message;
        }
    }
}

fn send(tx: &mpsc::UnboundedSender<Value>, id: u64, method: &str, params: Value) {
    tx.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        .expect("ACP input open");
}

fn assert_control_result(method: &str, result: &Value) {
    match method {
        "session/attach" | "session/takeover" => {
            assert_eq!(result["previous_controller_id"], "desktop");
            assert_eq!(result["active_controller_id"], "mobile");
            assert_eq!(result["client"]["prompt_injection"], true);
            assert_eq!(result["client"]["permission_routing"], true);
        }
        "session/live_clients" => {
            let clients = result["data"].as_array().expect("client list");
            assert_eq!(clients.len(), 2);
            assert!(clients.iter().any(|client| client["client_id"] == "mobile"));
        }
        "session/heartbeat" => {
            assert_eq!(result["client"]["client_id"], "mobile");
            assert_eq!(result["active_controller_id"], "desktop");
        }
        "session/detach" => {
            assert_eq!(result["active_controller_id"], "desktop");
            let clients = result["clients"].as_array().expect("remaining clients");
            assert_eq!(clients.len(), 1);
            assert_eq!(clients[0]["client_id"], "desktop");
        }
        _ => panic!("unmeasured control method: {method}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn queued_controls_respond_before_prompt_host_acknowledgement() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let directory = tempfile::tempdir().expect("fixture workspace");
            let pipeline = directory.path().join("held-at-host-manifest.harn");
            std::fs::write(
                &pipeline,
                "pipeline default(harness: Harness) { return \"completed\" }\n",
            )
            .expect("fixture pipeline");
            let cases = [
                ("session/attach", json!({"clientId":"mobile", "mode":"controller", "takeover":true})),
                ("session/takeover", json!({"clientId":"mobile"})),
                ("session/live_clients", json!({})),
                ("session/heartbeat", json!({"clientId":"mobile"})),
                ("session/detach", json!({"clientId":"mobile"})),
            ];
            for (method, mut params) in cases {
                let (tx, input) = mpsc::unbounded_channel();
                let (output, mut rx) = mpsc::unbounded_channel();
                let server = tokio::task::spawn_local(run_acp_channel_server(
                    AcpServerConfig::for_pipeline(pipeline.to_string_lossy().into_owned()),
                    input,
                    output,
                ));
                send(&tx, 1, "initialize", json!({"protocolVersion":1}));
                assert!(response(&mut rx, 1).await.get("error").is_none());
                send(&tx, 2, "session/new", json!({
                    "cwd":directory.path(), "environmentPolicy":{"kind":"isolated", "grants":[]}
                }));
                let created = response(&mut rx, 2).await;
                let session = created["result"]["sessionId"].as_str().expect("session identity");
                send(&tx, 3, "session/attach", json!({"sessionId":session, "clientId":"desktop", "mode":"controller"}));
                assert!(response(&mut rx, 3).await.get("error").is_none());
                send(&tx, 4, "session/attach", json!({"sessionId":session, "clientId":"mobile", "mode":"observer"}));
                assert!(response(&mut rx, 4).await.get("error").is_none());

                // Queue both frames before yielding to the real transport router.
                send(&tx, 5, "session/prompt", json!({"sessionId":session, "prompt":[{"type":"text", "text":"held"}]}));
                params["sessionId"] = json!(session);
                send(&tx, 6, method, params);
                let mut host_request = None;
                let mut control = None;
                while host_request.is_none() || control.is_none() {
                    let message = receive(&mut rx, &format!(
                        "queued {method}: host request {host_request:?}; control received {}",
                        control.is_some(),
                    )).await;
                    assert_ne!(message["id"], 5, "prompt completed before its host acknowledgement");
                    if message["method"] == "host/capabilities" {
                        assert!(host_request.is_none(), "duplicate host handshake");
                        host_request = Some(message["id"].clone());
                    } else if message["id"] == 6 {
                        assert!(message.get("error").is_none(), "{method}: {message}");
                        control = Some(message["result"].clone());
                    }
                }
                assert_control_result(method, &control.expect("control reached"));
                tx.send(json!({"jsonrpc":"2.0", "id":host_request.expect("prompt reached"), "result":{}}))
                    .expect("release host acknowledgement");
                let completed = response(&mut rx, 5).await;
                assert_eq!(completed["result"]["stopReason"], "end_turn");

                // Idle controls must not overtake an earlier close. This also
                // rejects a fix that simply preempts every known session.
                send(&tx, 7, "session/close", json!({"sessionId":session}));
                send(&tx, 8, "session/prompt", json!({"sessionId":session, "prompt":[{"type":"text", "text":"closed"}]}));
                send(&tx, 9, "session/takeover", json!({"sessionId":session, "clientId":"desktop"}));
                assert!(response(&mut rx, 7).await.get("error").is_none());
                assert_eq!(response(&mut rx, 8).await["error"]["code"], -32602);
                assert_eq!(response(&mut rx, 9).await["error"]["code"], -32602);
                drop(tx);
                harn_clock::test_support::within("control ingress shutdown", server)
                    .await
                    .expect("channel server settled");
            }
        })
        .await;
}
