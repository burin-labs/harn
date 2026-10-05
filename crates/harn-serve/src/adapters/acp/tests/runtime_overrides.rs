//! ACP proofs for per-server runtime configuration overrides.

use super::*;
use std::sync::Arc;
use tokio::task::LocalSet;

tokio::task_local! {
    static ENGINE_PROMPT_SCOPE: String;
}

struct ScopedRuntime {
    invoked: Arc<std::sync::atomic::AtomicUsize>,
    refuse: bool,
}

#[async_trait::async_trait(?Send)]
impl AcpRuntimeConfigurator for ScopedRuntime {
    async fn run_prompt(
        &self,
        context: AcpPromptExecutionContext<'_>,
        execution: AcpPromptExecution<'_>,
    ) -> Result<String, AcpPromptExecutionError> {
        assert!(!context.session_id.is_empty());
        assert!(context.cwd.is_absolute());
        assert!(context.project_root.is_absolute());
        assert!(!context.is_cancelled());
        if self.refuse {
            return Err("fixture authority refused before execution"
                .to_string()
                .into());
        }
        // Rc retained over this await proves the configured engine seam is local.
        let local = std::rc::Rc::new(context.session_id.to_string());
        tokio::task::yield_now().await;
        ENGINE_PROMPT_SCOPE.scope((*local).clone(), execution).await
    }

    fn configure_harness(&self, harness: harn_vm::Harness) -> harn_vm::Harness {
        ENGINE_PROMPT_SCOPE.with(|session| assert!(!session.is_empty()));
        self.invoked.fetch_add(1, Ordering::SeqCst);
        harness
    }
}

#[test]
fn acp_manifest_advertises_local_runtime_prompt_content() {
    let manifest = super::super::builtins::advertise_runtime_prompt_content(VmValue::dict_map(
        Default::default(),
    ));
    let operations = manifest
        .as_dict()
        .and_then(|root| root.get("runtime"))
        .and_then(|value| value.as_dict())
        .and_then(|runtime| runtime.get("ops"))
        .and_then(|value| match value {
            VmValue::List(values) => Some(values),
            _ => None,
        })
        .expect("runtime operation list");

    assert!(operations
        .iter()
        .any(|value| value.display() == "prompt_content"));
}

#[tokio::test(flavor = "current_thread")]
async fn acp_provider_catalog_method_matches_export_artifact_with_overrides() {
    let _reset = crate::test_support::LlmOverrideReset;
    let overlay = crate::test_support::fixture_provider_overlay();
    let capability_overlay = crate::test_support::fixture_capability_overlay();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(
        AcpServerConfig::new(None)
            .with_llm_overrides(Some(overlay.clone()), Some(capability_overlay.clone())),
        AcpOutput::Channel(tx),
    );

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": HARN_PROVIDER_CATALOG_METHOD,
            "params": {},
        }))
        .await;
    let response = recv_json(&mut rx).await;
    let expected = serde_json::to_value(harn_vm::provider_catalog::artifact_with_overrides(
        Some(&overlay),
        Some(&capability_overlay),
    ))
    .expect("expected catalog json");
    assert_eq!(response["result"], expected);
    assert!(response["result"]["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .any(|provider| provider["id"] == "fixture_runtime"));
}

fn endpoint_overlay() -> harn_vm::llm_config::ProvidersConfig {
    harn_vm::llm_config::parse_config_toml(
        r#"
[providers.fixture]
base_url = "http://catalog.invalid/v1"
auth_style = "none"

[providers.fixture.healthcheck]
method = "GET"
path = "/health"
"#,
    )
    .expect("fixture provider overlay parses")
}

async fn start_health_endpoint() -> (String, tokio::task::JoinHandle<()>) {
    let app = axum::Router::new().route(
        "/health",
        axum::routing::get(|| async { axum::http::StatusCode::NO_CONTENT }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("health listener");
    let address = listener.local_addr().expect("health listener address");
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("health endpoint serves");
    });
    (format!("http://{address}"), task)
}

async fn prompt_healthcheck(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    response_rx: &mut mpsc::UnboundedReceiver<String>,
    session_id: &str,
    request_id: i64,
) -> String {
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{
                    "type": "text",
                    "text": "const result = harness.llm.healthcheck(\"fixture\")\nharness.stdio.println(result.valid)\nharness.stdio.println(result.metadata.url)",
                }],
            },
        }))
        .expect("send healthcheck prompt");

    let mut output = String::new();
    loop {
        let message = recv_json(response_rx).await;
        if message["method"] == "host/capabilities" {
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": message["id"].clone(),
                    "result": {},
                }))
                .expect("send host capabilities response");
            continue;
        }
        if message["method"] == "session/update"
            && message["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
        {
            if let Some(text) = message["params"]["update"]["content"]["text"].as_str() {
                output.push_str(text);
            }
        }
        if message["id"] == serde_json::json!(request_id) {
            return output;
        }
    }
}

async fn set_code_mode(
    request_tx: &mpsc::UnboundedSender<serde_json::Value>,
    response_rx: &mut mpsc::UnboundedReceiver<String>,
    session_id: &str,
    request_id: i64,
) {
    request_tx
        .send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "session/set_mode",
            "params": {"sessionId": session_id, "modeId": "code"},
        }))
        .expect("send session/set_mode");

    let ack = recv_json(response_rx).await;
    assert_eq!(ack["id"], request_id);
    for expected in ["current_mode_update", "config_option_update"] {
        let notification = recv_json(response_rx).await;
        assert_eq!(notification["method"], "session/update");
        assert_eq!(notification["params"]["update"]["sessionUpdate"], expected);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn acp_runtime_provider_endpoints_are_scoped_per_live_server() {
    let local = LocalSet::new();
    local
        .run_until(async {
            let (first_endpoint, first_endpoint_task) = start_health_endpoint().await;
            let (second_endpoint, second_endpoint_task) = start_health_endpoint().await;
            let invoked = Arc::new(std::sync::atomic::AtomicUsize::new(0));

            let (first_tx, mut first_rx, first_server, first_session) =
                start_acp_channel_session_with_config(
                    AcpServerConfig::new(None)
                        .with_llm_overrides(Some(endpoint_overlay()), None)
                        .with_runtime_provider_endpoint("fixture", &first_endpoint)
                        .expect("first endpoint override")
                        .with_runtime_configurator(Arc::new(ScopedRuntime {
                            invoked: invoked.clone(),
                            refuse: false,
                        })),
                    serde_json::json!("."),
                )
                .await;
            let (second_tx, mut second_rx, second_server, second_session) =
                start_acp_channel_session_with_config(
                    AcpServerConfig::new(None)
                        .with_llm_overrides(Some(endpoint_overlay()), None)
                        .with_runtime_provider_endpoint("fixture", &second_endpoint)
                        .expect("second endpoint override"),
                    serde_json::json!("."),
                )
                .await;

            tokio::join!(
                set_code_mode(&first_tx, &mut first_rx, &first_session, 8),
                set_code_mode(&second_tx, &mut second_rx, &second_session, 9),
            );
            let (first_output, second_output) = tokio::join!(
                prompt_healthcheck(&first_tx, &mut first_rx, &first_session, 10),
                prompt_healthcheck(&second_tx, &mut second_rx, &second_session, 20),
            );
            assert!(
                first_output.contains(&format!("{first_endpoint}/health")),
                "first server must use its verified endpoint: {first_output}"
            );
            assert!(
                first_output.contains("true"),
                "first server healthcheck must succeed: {first_output}"
            );
            assert!(
                second_output.contains(&format!("{second_endpoint}/health")),
                "second server must use its verified endpoint: {second_output}"
            );
            assert!(
                second_output.contains("true"),
                "second server healthcheck must succeed: {second_output}"
            );
            assert_eq!(invoked.load(Ordering::SeqCst), 1);
            assert!(ENGINE_PROMPT_SCOPE.try_with(|_| ()).is_err());

            drop(first_tx);
            drop(second_tx);
            first_server.abort();
            second_server.abort();
            first_endpoint_task.abort();
            second_endpoint_task.abort();
            let _ = first_server.await;
            let _ = second_server.await;
            let _ = first_endpoint_task.await;
            let _ = second_endpoint_task.await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_runtime_authority_refusal_never_polls_the_engine_future() {
    LocalSet::new()
        .run_until(async {
            let invoked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (tx, mut rx, server, session) = start_acp_channel_session_with_config(
                AcpServerConfig::new(None).with_runtime_configurator(Arc::new(ScopedRuntime {
                    invoked: invoked.clone(),
                    refuse: true,
                })),
                serde_json::json!("."),
            )
            .await;
            set_code_mode(&tx, &mut rx, &session, 8).await;
            tx.send(serde_json::json!({
                "jsonrpc": "2.0", "id": 10, "method": "session/prompt",
                "params": {
                    "sessionId": session,
                    "prompt": [{"type": "text", "text": "harness.stdio.println(\"must not execute\")"}],
                },
            }))
            .unwrap();
            loop {
                let response = recv_json(&mut rx).await;
                if response["id"] == 10 {
                    assert!(response["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("fixture authority refused before execution"));
                    assert!(response.get("result").is_none());
                    break;
                }
            }
            assert_eq!(invoked.load(Ordering::SeqCst), 0);
            drop(tx);
            server.abort();
            let _ = server.await;
        })
        .await;
}
