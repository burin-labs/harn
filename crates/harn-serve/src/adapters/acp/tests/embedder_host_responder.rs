//! A server configured with a host responder answers a session's host
//! requests itself, so a client that is not a host can still run a turn.

use super::*;
use std::sync::atomic::AtomicUsize;

struct FixtureHost {
    capabilities: AtomicUsize,
    calls: AtomicUsize,
    mcp_config: AtomicUsize,
}

impl HostResponder for FixtureHost {
    fn respond(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, AcpJsonRpcError> {
        match method {
            "host/capabilities" => {
                self.capabilities.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({"project": ["metadata_get", "mcp_config"]}))
            }
            // The session boot asks for host MCP servers outside the pipeline.
            "host/call" if params["name"] == "project.mcp_config" => {
                self.mcp_config.fetch_add(1, Ordering::SeqCst);
                assert!(params["sessionId"].is_string());
                Ok(serde_json::json!([]))
            }
            "host/call" if params["name"] == "project.metadata_get" => {
                self.calls.fetch_add(1, Ordering::SeqCst);
                assert!(params["sessionId"].is_string());
                Ok(serde_json::json!({"proof": {"owner": "embedder"}}))
            }
            _ => Err(AcpJsonRpcError {
                code: -32601,
                message: format!("fixture refuses {}", params["name"]),
                data: None,
            }),
        }
    }
}

/// Run one inline turn and collect every request the server sent the client.
async fn run_turn(config: AcpServerConfig) -> (serde_json::Value, Vec<String>) {
    let (request_tx, mut response_rx, _server, session_id) =
        start_acp_channel_session_with_config(config, serde_json::json!(".")).await;
    let source = r#"const facts = harness.project.metadata_get({dir: "proof"})
assert(facts.proof.owner == "embedder", "the embedder must answer the host call")
harness.stdio.println("embedder-host-observed")
"#;
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0", "id": 9, "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": source}]},
        }))
        .expect("send session/prompt");
    let mut client_requests = Vec::new();
    let response = harn_clock::test_support::within("session/prompt response", async {
        loop {
            let line = response_rx.recv().await.expect("ACP channel open");
            let message: serde_json::Value = serde_json::from_str(&line).expect("ACP JSON");
            if message["id"] == 9 && message.get("method").is_none() {
                return message;
            }
            if let (Some(method), Some(_)) = (message["method"].as_str(), message.get("id")) {
                client_requests.push(method.to_string());
                // Answer as a plain editor would: it is not a host.
                request_tx
                    .send(serde_json::json!({"jsonrpc": "2.0", "id": message["id"],
                        "error": {"code": -32601, "message": "Method not found"}}))
                    .expect("answer client request");
            }
        }
    })
    .await;
    (response, client_requests)
}

#[tokio::test(flavor = "current_thread")]
async fn a_configured_responder_answers_host_requests_instead_of_the_client() {
    harn_clock::test_support::with_local_tasks(async {
        harn_vm::reset_thread_local_state();
        let responder = Arc::new(FixtureHost {
            capabilities: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            mcp_config: AtomicUsize::new(0),
        });
        let (response, client_requests) =
            run_turn(AcpServerConfig::new(None).with_host_responder(responder.clone())).await;
        assert!(response["error"].is_null(), "turn failed: {response}");
        assert_eq!(responder.calls.load(Ordering::SeqCst), 1);
        assert_eq!(responder.mcp_config.load(Ordering::SeqCst), 1);
        assert!(responder.capabilities.load(Ordering::SeqCst) >= 1);
        assert!(
            !client_requests
                .iter()
                .any(|method| method.starts_with("host/")),
            "host requests must not reach the client: {client_requests:?}"
        );

        // Negative control: without a responder the same turn asks the client,
        // which cannot answer, so it fails.
        harn_vm::reset_thread_local_state();
        let (response, client_requests) = run_turn(AcpServerConfig::new(None)).await;
        assert!(!response["error"].is_null(), "turn must fail: {response}");
        assert!(client_requests.iter().any(|method| method == "host/call"));
    })
    .await;
}
