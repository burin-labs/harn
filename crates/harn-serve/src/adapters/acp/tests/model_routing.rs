use super::*;
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn qualified_session_models_reach_the_registered_provider() {
    let _reset = crate::test_support::LlmOverrideReset;
    let _env = EnvSnapshot::capture(&["HARN_LLM_CALLS_DISABLED", "HARN_STATE_DIR"]);
    std::env::set_var("HARN_LLM_CALLS_DISABLED", "0");
    tokio::task::LocalSet::new()
        .run_until(async {
            for provider in ["local", "llamacpp", "ollama"] {
                for separator in [":", "/"] {
                    assert_provider_request(provider, separator).await;
                }
            }
        })
        .await;
}

async fn assert_provider_request(provider: &str, separator: &str) {
    let (body_tx, mut body_rx) = mpsc::unbounded_channel();
    let openai_tx = body_tx.clone();
    let app = axum::Router::new()
        .route(
            "/v1/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let body_tx = openai_tx.clone();
                async move {
                    body_tx.send(("openai", body.clone())).unwrap();
                    axum::Json(json!({
                        "id": "route-proof", "object": "chat.completion", "model": body["model"],
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": "route fired"}, "finish_reason": "stop"}],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                }
            }),
        )
        .route(
            "/api/chat",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let body_tx = body_tx.clone();
                async move {
                    body_tx.send(("ollama", body.clone())).unwrap();
                    axum::Json(json!({
                        "model": body["model"], "message": {"role": "assistant", "content": "route fired"},
                        "done": true, "prompt_eval_count": 1, "eval_count": 1
                    }))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let http = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("HARN_STATE_DIR", dir.path().join(".harn"));
    let pipeline = dir.path().join("model-route.harn");
    std::fs::write(
        &pipeline,
        r#"pipeline main(harness: Harness) {
  const reply = harness.llm.call("route proof", nil, {max_tokens: 16, retries: 0, stream: false})
  harness.stdio.println(reply.text)
}
"#,
    )
    .unwrap();
    let providers = harn_vm::llm_config::parse_config_toml(
        "[providers.local]\nauth_style = \"none\"\n[providers.llamacpp]\nauth_style = \"none\"\n[providers.ollama]\nauth_style = \"none\"\n",
    )
    .unwrap();
    let mut config = AcpServerConfig::for_pipeline(pipeline.to_string_lossy().into_owned())
        .with_llm_overrides(Some(providers), None);
    for name in ["local", "llamacpp", "ollama"] {
        config = config
            .with_runtime_provider_endpoint(name, &endpoint)
            .unwrap();
    }
    let (tx, mut rx, server, session_id) =
        start_acp_code_session_with_config(config, json!(dir.path())).await;
    let model = format!("{provider}-route-proof");
    tx.send(json!({
        "jsonrpc": "2.0", "id": 3, "method": "session/set_config_option",
        "params": {"sessionId": session_id, "configId": "model", "value": format!("{provider}{separator}{model}")}
    }))
    .unwrap();
    let pinned = recv_response_with_id(&mut rx, 3).await;
    assert!(pinned.get("error").is_none(), "pin refused: {pinned}");
    let response =
        super::served_agent_turn::prompt(&tx, &mut rx, &session_id, 4, "Use the selected route")
            .await;
    assert!(response.get("error").is_none(), "prompt failed: {response}");
    assert_eq!(response["result"]["stopReason"], "end_turn");
    let (wire, body) = body_rx.try_recv().expect("actual provider request reached");
    assert_eq!(body["model"], model);
    assert_eq!(
        wire,
        if provider == "ollama" {
            "ollama"
        } else {
            "openai"
        }
    );
    assert!(body_rx.try_recv().is_err(), "exactly one provider call");
    println!("ACP selected {provider}:{model}; actual {wire} request matched");
    server.abort();
    http.abort();
}

#[tokio::test(flavor = "current_thread")]
async fn replay_fixture_provider_is_admitted_only_inside_its_mock_scope() {
    let _reset = crate::test_support::LlmOverrideReset;
    harn_vm::llm::clear_cli_llm_mock_mode();
    assert!(crate::adapters::acp::modes::validate_model_selector("fixture:scoped-model").is_err());
    let mock = harn_vm::llm::parse_llm_mock_value(&json!({
        "provider": "fixture", "model": "scoped-model", "text": "scoped reply"
    }))
    .unwrap();
    harn_vm::llm::install_cli_llm_mocks(vec![mock]);
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let pipeline = dir.path().join("replay.harn");
            std::fs::write(&pipeline, r#"pipeline main(harness: Harness) {
  const reply = harness.llm.call("scoped route", nil, {max_tokens: 16, stream: false})
  assert(reply.text == "scoped reply")
  assert(len(harness.llm.mock_calls()) == 1)
  harness.stdio.println(reply.text)
}
"#).unwrap();
            let config = AcpServerConfig::for_pipeline(pipeline.to_string_lossy().into_owned());
            let (tx, mut rx, server, session_id) =
                start_acp_code_session_with_config(config, json!(dir.path())).await;
            tx.send(json!({
                "jsonrpc": "2.0", "id": 3, "method": "session/set_config_option",
                "params": {"sessionId": session_id, "configId": "model", "value": "fixture:scoped-model"}
            })).unwrap();
            let pinned = recv_response_with_id(&mut rx, 3).await;
            assert!(pinned.get("error").is_none(), "scoped pin refused: {pinned}");
            let response = super::served_agent_turn::prompt(&tx, &mut rx, &session_id, 4, "Use replay").await;
            assert!(response.get("error").is_none(), "scoped prompt failed: {response}");
            assert_eq!(response["result"]["stopReason"], "end_turn");
            server.abort();
        })
        .await;
}
