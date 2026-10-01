use super::*;
use axum::response::IntoResponse;
use harn_session_store::{CreateSession, ReadRange, SessionStore};
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn acp_session_projects_the_reasoning_level_that_left_for_the_provider() {
    let _reset = crate::test_support::LlmOverrideReset;
    let _env = EnvSnapshot::capture(&["HARN_LLM_CALLS_DISABLED", "HARN_STATE_DIR"]);
    std::env::set_var("HARN_LLM_CALLS_DISABLED", "0");
    for cancelled in [false, true] {
        tokio::task::LocalSet::new()
            .run_until(assert_acp_reasoning_receipt(cancelled))
            .await;
    }
}

async fn assert_acp_reasoning_receipt(cancelled: bool) {
    let (body_tx, mut body_rx) = mpsc::unbounded_channel();
    let handler = move |axum::Json(body): axum::Json<serde_json::Value>| {
        let body_tx = body_tx.clone();
        async move {
            let streaming = body["stream"] == true;
            body_tx.send(body).expect("capture request");
            if cancelled {
                std::future::pending::<()>().await;
            }
            let response = json!({
                "id": "reasoning-proof", "status": "completed", "model": "gemini-3.6-flash",
                "steps": [{"type": "model_output", "content": [{"type": "text", "text": "all done"}]}],
                "usage": {"total_input_tokens": 1, "total_output_tokens": 2},
            });
            if streaming {
                let events = [
                    json!({"event_type": "step.start", "index": 0, "step": {"type": "model_output"}}),
                    json!({"event_type": "step.delta", "index": 0, "delta": {"text": "all done"}}),
                    json!({"event_type": "step.stop", "index": 0}),
                    json!({"event_type": "interaction.completed", "interaction": response}),
                ];
                let stream: String = events
                    .iter()
                    .map(|event| format!("data: {event}\n\n"))
                    .collect();
                ([("content-type", "text/event-stream")], stream).into_response()
            } else {
                axum::Json(response).into_response()
            }
        }
    };
    let app = axum::Router::new().route("/v1beta/interactions", axum::routing::post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let http = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    // The isolated ACP environment hides session-store overrides.
    // Pin the runtime's default state root, which also applies under Nextest.
    std::env::set_var("HARN_STATE_DIR", dir.path().join(".harn"));
    let pipeline = dir.path().join("reasoning.harn");
    std::fs::write(
        &pipeline,
        r#"import { agent_loop } from "std/agent/loop"
pipeline main(harness: Harness) {
  agent_loop(harness, prompt, nil, {
    provider: "gemini", model: "gemini-3.6-flash", max_iterations: 1,
    llm_options: {stream: false, thinking: {mode: "effort", level: "xhigh"}},
  })
}
"#,
    )
    .unwrap();
    let providers = harn_vm::llm_config::parse_config_toml(
        r#"
[providers.gemini]
auth_style = "none"
"#,
    )
    .unwrap();
    let config = AcpServerConfig::for_pipeline(pipeline.to_string_lossy().into_owned())
        .with_llm_overrides(Some(providers), None)
        .with_runtime_provider_endpoint("gemini", &endpoint)
        .unwrap();
    let (tx, mut rx, server, session_id) =
        start_acp_channel_session_with_config(config, json!(dir.path())).await;
    let store = harn_vm::open_canonical_store(dir.path()).unwrap();
    store
        .create(CreateSession {
            id: Some(session_id.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    let (response, body) = {
        let prompt =
            super::served_agent_turn::prompt(&tx, &mut rx, &session_id, 2, "Reply briefly");
        tokio::pin!(prompt);
        if cancelled {
            let body = tokio::select! {
                body = body_rx.recv() => body.expect("provider request was reached"),
                response = &mut prompt => panic!("prompt finished before cancellation: {response}"),
            };
            tx.send(json!({
                "jsonrpc": "2.0", "method": "session/cancel",
                "params": {"sessionId": session_id},
            }))
            .unwrap();
            (prompt.await, body)
        } else {
            let response = prompt.await;
            (
                response,
                body_rx.try_recv().expect("provider request was reached"),
            )
        }
    };
    assert!(
        response.get("error").is_none(),
        "ACP prompt failed: {response}"
    );
    assert_eq!(
        response["result"]["stopReason"],
        if cancelled { "cancelled" } else { "end_turn" }
    );
    assert_eq!(body["generation_config"]["thinking_level"], "high");
    assert!(body_rx.try_recv().is_err(), "exactly one provider call");
    let journal = store.read(&session_id, ReadRange::default()).await.unwrap();
    assert!(
        journal.events.iter().any(|event| {
            event.kind.discriminator() == "agent_run_started"
                && event.payload["transcript_event"]["metadata"]["reasoning_receipts_reported"]
                    == true
        }),
        "the live journal must identify a reporting producer, including for zero-call runs: {:?}",
        journal.events
    );
    assert_eq!(
        journal
            .events
            .iter()
            .filter(|event| event.kind.discriminator() == "reasoning_receipt")
            .count(),
        1
    );
    let path =
        harn_vm::orchestration::materialize_session_run_record(dir.path(), &session_id, None)
            .await
            .unwrap();
    let run: harn_vm::orchestration::RunRecord =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let receipts = run.evidence.reasoning_receipts.expect("measured receipts");
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].resolved_level.as_deref(), Some("xhigh"));
    assert_eq!(receipts[0].sent_value, Some(json!("high")));
    assert_eq!(receipts[0].wire_dialect, "gemini_interactions");
    drop(tx);
    server.await.unwrap();
    http.abort();
    let _ = http.await;
}
