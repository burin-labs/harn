//! A server configured with a host responder answers a session's host
//! requests itself, so a client that is not a host can still run a turn, and
//! each session is answered for its own workspace root.

use super::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;

#[derive(Default)]
struct Counts {
    capabilities: AtomicUsize,
    calls: AtomicUsize,
    mcp_config: AtomicUsize,
    bound_roots: Mutex<Vec<PathBuf>>,
}

/// A responder bound to one root; it answers with that root.
struct FixtureHost {
    root: PathBuf,
    counts: Arc<Counts>,
}

impl HostResponder for FixtureHost {
    fn respond(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<serde_json::Value, AcpJsonRpcError> {
        let counts = &self.counts;
        match method {
            "host/capabilities" => {
                counts.capabilities.fetch_add(1, Ordering::SeqCst);
                Ok(serde_json::json!({"project": ["metadata_get", "mcp_config"]}))
            }
            // The session boot asks for host MCP servers outside the pipeline.
            "host/call" if params["name"] == "project.mcp_config" => {
                counts.mcp_config.fetch_add(1, Ordering::SeqCst);
                assert!(params["sessionId"].is_string());
                Ok(serde_json::json!([]))
            }
            "host/call" if params["name"] == "project.metadata_get" => {
                counts.calls.fetch_add(1, Ordering::SeqCst);
                assert!(params["sessionId"].is_string());
                Ok(serde_json::json!({"proof": {"owner": "embedder",
                    "root": self.root.display().to_string()}}))
            }
            _ => Err(AcpJsonRpcError {
                code: -32601,
                message: format!("fixture refuses {}", params["name"]),
                data: None,
            }),
        }
    }
}

struct FixtureBinder(Arc<Counts>);

impl HostResponderBinder for FixtureBinder {
    fn bind(&self, session_root: &Path) -> Result<Arc<dyn HostResponder>, String> {
        self.0
            .bound_roots
            .lock()
            .unwrap()
            .push(session_root.to_path_buf());
        Ok(Arc::new(FixtureHost {
            root: session_root.to_path_buf(),
            counts: self.0.clone(),
        }))
    }
}

/// Run one inline turn in `session_id`, answering client requests as a plain
/// editor would, and return the response, client requests and printed text.
async fn run_turn(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    response_rx: &mut mpsc::UnboundedReceiver<String>,
    session_id: &str,
    id: u64,
) -> (serde_json::Value, Vec<String>, String) {
    let source = r#"const facts = harness.project.metadata_get({dir: "proof"})
assert(facts.proof.owner == "embedder", "the embedder must answer the host call")
harness.stdio.println("root=" + facts.proof.root)
"#;
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": source}]},
        }))
        .expect("send session/prompt");
    let mut client_requests = Vec::new();
    let mut printed = String::new();
    let response = harn_clock::test_support::within("session/prompt response", async {
        loop {
            let line = response_rx.recv().await.expect("ACP channel open");
            let message: serde_json::Value = serde_json::from_str(&line).expect("ACP JSON");
            if message["id"] == id && message.get("method").is_none() {
                return message;
            }
            if let Some(text) = message["params"]["update"]["content"]["text"].as_str() {
                printed.push_str(text);
            }
            if let (Some(method), Some(_)) = (message["method"].as_str(), message.get("id")) {
                client_requests.push(method.to_string());
                // A plain editor is not a host.
                request_tx
                    .send(serde_json::json!({"jsonrpc": "2.0", "id": message["id"],
                        "error": {"code": -32601, "message": "Method not found"}}))
                    .expect("answer client request");
            }
        }
    })
    .await;
    (response, client_requests, printed)
}

fn new_session_params(cwd: &Path) -> serde_json::Value {
    serde_json::json!({"cwd": cwd, "environmentPolicy": {"kind": "isolated", "grants": []}})
}

#[tokio::test(flavor = "current_thread")]
async fn each_session_is_answered_by_a_responder_bound_to_its_own_root() {
    harn_clock::test_support::with_local_tasks(async {
        harn_vm::reset_thread_local_state();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let counts = Arc::new(Counts::default());
        let config =
            AcpServerConfig::new(None).with_host_responder(Arc::new(FixtureBinder(counts.clone())));
        let (request_tx, mut response_rx, _server, first_session) =
            start_acp_channel_session_with_params(config, new_session_params(first.path())).await;
        request_tx
            .send(
                serde_json::json!({"jsonrpc": "2.0", "id": 5, "method": "session/new",
                "params": new_session_params(second.path())}),
            )
            .expect("send second session/new");
        let created = recv_response_with_id(&mut response_rx, 5).await;
        let second_session = created["result"]["sessionId"].as_str().unwrap().to_string();

        let mut printed = Vec::new();
        for (index, session) in [&first_session, &second_session].into_iter().enumerate() {
            let (response, client_requests, text) =
                run_turn(&request_tx, &mut response_rx, session, 10 + index as u64).await;
            assert!(response["error"].is_null(), "turn failed: {response}");
            assert!(
                !client_requests
                    .iter()
                    .any(|method| method.starts_with("host/")),
                "host requests must not reach the client: {client_requests:?}"
            );
            printed.push(text);
        }
        assert_eq!(counts.calls.load(Ordering::SeqCst), 2);
        assert_eq!(counts.mcp_config.load(Ordering::SeqCst), 2);
        assert!(counts.capabilities.load(Ordering::SeqCst) >= 2);
        let roots = [first.path(), second.path()].map(|root| root.canonicalize().unwrap());
        assert_eq!(*counts.bound_roots.lock().unwrap(), roots.to_vec());
        for (text, root) in printed.iter().zip(&roots) {
            assert!(
                text.contains(&format!("root={}", root.display())),
                "a session must be answered for its own root: {text:?} vs {root:?}"
            );
        }
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn without_a_responder_host_requests_go_to_the_client() {
    harn_clock::test_support::with_local_tasks(async {
        harn_vm::reset_thread_local_state();
        let root = tempfile::tempdir().unwrap();
        let (request_tx, mut response_rx, _server, session) =
            start_acp_channel_session_with_params(
                AcpServerConfig::new(None),
                new_session_params(root.path()),
            )
            .await;
        let (response, client_requests, _) =
            run_turn(&request_tx, &mut response_rx, &session, 9).await;
        assert!(!response["error"].is_null(), "turn must fail: {response}");
        assert!(client_requests.iter().any(|method| method == "host/call"));
    })
    .await;
}
