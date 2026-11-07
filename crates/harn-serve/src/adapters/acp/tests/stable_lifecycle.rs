use super::*;
use harn_session_store::SessionStore;
use serde_json::json;

fn validator() -> jsonschema::Validator {
    let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/protocols/schemas/acp-1.26.0-upstream.schema.json"
    )))
    .unwrap();
    jsonschema::draft202012::new(&json!({
        "$ref":"#/$defs/SessionNotification", "$defs":schema["$defs"]
    }))
    .unwrap()
}

#[test]
fn malformed_optional_grants_and_extension_names_cannot_enable_standard_session_capabilities() {
    for session in [
        json!(null),
        json!({}),
        json!({"notices":true,"compaction":[]}),
    ] {
        let capabilities =
            super::super::client_capabilities::ClientCapabilities::from_initialize(&json!({
                "clientCapabilities":{"session":session,"_meta":{"harn":{
                    "sessionUpdateExtensions":["notice","compaction_update"]
                }}}
            }));
        assert!(capabilities.supports_session_update("usage_update"));
        assert!(!capabilities.supports_session_update("notice"));
        assert!(!capabilities.supports_session_update("compaction_update"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn actual_compaction_commit_failure_cancellation_and_noop_obey_client_grants() {
    harn_clock::test_support::with_local_tasks(async {
        let validator = validator();
        for granted in [false, true] {
            for mode in ["native", "auto", "emergency", "noop", "fail", "cancel"] {
                let root = tempfile::tempdir().unwrap();
                let pipeline = root.path().join("compaction.harn");
                std::fs::write(&pipeline, include_str!("fixtures/stable_compaction.harn")).unwrap();
                let (tx, mut rx, server, session) = start_acp_channel_session_with_config(
                    AcpServerConfig::for_pipeline(pipeline.to_string_lossy().into_owned()),
                    json!(root.path()),
                ).await;
                tx.send(json!({"jsonrpc":"2.0","id":10,"method":"initialize","params":{
                    "clientCapabilities": {
                        "session":if granted {json!({"notices":{},"compaction":{}})} else {json!({})},
                        // An extension grant must never bypass the standard capability gate.
                        "_meta":{"harn":{"sessionUpdateExtensions":["notice","compaction_update","transcript_compacted"]}}
                    }
                }})).unwrap();
                assert!(recv_response_with_id(&mut rx, 10).await.get("result").is_some());
                tx.send(json!({"jsonrpc":"2.0","id":11,"method":"session/set_mode","params":{
                    "sessionId":session,"modeId":"code"
                }})).unwrap();
                assert!(recv_response_with_id(&mut rx, 11).await.get("result").is_some());
                tx.send(json!({"jsonrpc":"2.0","id":12,"method":"session/prompt","params":{
                    "sessionId":session,"prompt":[{"type":"text","text":format!("{session}|{mode}")}]
                }})).unwrap();
                let mut compact = Vec::new();
                let mut notices = Vec::new();
                let mut committed = Vec::new();
                let mut cancelled_at_host = false;
                let response = loop {
                    let frame = recv_json(&mut rx).await;
                    if frame["method"] == "host/capabilities" { reply_empty_result(&tx, &frame); }
                    if frame["method"] == "host/call" {
                        assert_eq!(mode, "cancel", "unexpected host effect: {frame}");
                        assert_eq!(frame["params"]["name"], "interaction.ask");
                        cancelled_at_host = true;
                        tx.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}})).unwrap();
                    }
                    if frame["method"] == "session/update" {
                        let update = &frame["params"]["update"];
                        match update["sessionUpdate"].as_str() {
                            Some("compaction_update" | "notice") => {
                                assert!(granted, "ungranted standard update escaped: {frame}");
                                assert!(validator.is_valid(&frame["params"]), "{frame}");
                                let mut malformed = frame["params"].clone();
                                let required = if update["sessionUpdate"] == "notice" {"title"} else {"compactionId"};
                                malformed["update"].as_object_mut().unwrap().remove(required);
                                assert!(!validator.is_valid(&malformed));
                                if update["sessionUpdate"] == "notice" {notices.push(update.clone());}
                                else {compact.push(update.clone());}
                            }
                            Some("transcript_compacted") => committed.push(update.clone()),
                            _ => {}
                        }
                    }
                    if frame["id"] == 12 {break frame;}
                };
                assert_eq!(cancelled_at_host, mode == "cancel", "{response}");
                if matches!(mode, "fail") {
                    assert!(response.get("error").is_some(), "{response}");
                    assert!(response["error"]["message"].as_str().unwrap().contains("controlled compaction strategy failure"), "{response}");
                } else {
                    assert_eq!(response["result"]["stopReason"], if mode == "cancel" {"cancelled"} else {"end_turn"}, "{response}");
                }
                assert_eq!(notices.len(), usize::from(granted));
                let applied = matches!(mode, "native" | "auto" | "emergency");
                assert_eq!(committed.len(), usize::from(applied), "mode={mode}: {committed:?}");
                assert_eq!(compact.len(), if granted && mode != "noop" {2} else {0}, "mode={mode}: {compact:?}");
                if !compact.is_empty() {
                    assert_eq!(compact[0]["status"], "in_progress");
                    assert_eq!(compact[1]["status"], match mode {"fail"=>"failed","cancel"=>"cancelled",_=>"completed"});
                    assert_eq!(compact[0]["compactionId"], compact[1]["compactionId"]);
                    if applied {assert_eq!(compact[1]["compactionId"], committed[0]["_meta"]["harn"]["receiptId"]);}
                }
                drop(tx);
                server.await.unwrap();
            }
        }
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn actual_provider_usage_reports_resident_context_and_cumulative_session_cost_without_fake_zero(
) {
    let _reset = crate::test_support::LlmOverrideReset;
    let _env = EnvSnapshot::capture(&["HARN_LLM_CALLS_DISABLED", "HARN_STATE_DIR"]);
    std::env::set_var("HARN_LLM_CALLS_DISABLED", "0");
    harn_clock::test_support::with_local_tasks(async {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let app = axum::Router::new().route("/v1beta/interactions", axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
            let seen = seen.clone();
            async move {
                assert_eq!(body["stream"], false);
                let call = seen.fetch_add(1, Ordering::SeqCst);
                let mut response = json!({
                    "id":format!("usage-{call}"),"status":"completed","model":"gemini-3.6-flash",
                    "steps":[{"type":"model_output","content":[{"type":"text","text":"measured reply"}]}]
                });
                // A reported zero is priced; missing usage is unmeasured.
                if call < 3 {
                    response["usage"] = json!({"total_input_tokens":if call == 0 {0}else{100000},"total_output_tokens":0});
                }
                axum::Json(response)
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let http = tokio::spawn(async move {axum::serve(listener, app).await.unwrap()});
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("HARN_STATE_DIR", root.path().join(".harn"));
        let pipeline = root.path().join("usage.harn");
        std::fs::write(&pipeline, include_str!("fixtures/stable_usage.harn")).unwrap();
        let providers = harn_vm::llm_config::parse_config_toml("[providers.gemini]\nauth_style = \"none\"\n").unwrap();
        let config = AcpServerConfig::for_pipeline(pipeline.to_string_lossy().into_owned())
            .with_llm_overrides(Some(providers), None)
            .with_runtime_provider_endpoint("gemini", &endpoint).unwrap();
        let (tx, mut rx, server, session) = start_acp_channel_session_with_config(config, json!(root.path())).await;
        let store = harn_vm::open_canonical_store(root.path()).unwrap();
        assert_eq!(store.describe(&session).await.unwrap().id, session);
        let validator = validator();
        let mut prior = 0.0;
        for call in 0..4 {
            let id = 20 + call;
            tx.send(json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":{
                "sessionId":session,"prompt":[{"type":"text","text":"Reply briefly"}]
            }})).unwrap();
            let mut updates = Vec::new();
            loop {
                let frame = recv_json(&mut rx).await;
                if frame["method"] == "host/capabilities" {reply_empty_result(&tx, &frame);}
                if frame["params"]["update"]["sessionUpdate"] == "usage_update" {
                    assert!(validator.is_valid(&frame["params"]), "{frame}");
                    updates.push(frame["params"]["update"].clone());
                }
                if frame["id"] == id {
                    assert_eq!(frame["result"]["stopReason"], "end_turn", "{frame}");
                    break;
                }
            }
            assert_eq!(calls.load(Ordering::SeqCst), call as usize + 1, "each prompt must reach exactly one actual HTTP request");
            let update = updates.last().expect("the real provider attempt must publish stable usage");
            assert!(update["used"].as_u64().is_some_and(|tokens| tokens > 0 && tokens < 100000), "current resident context, not cumulative provider input: {update}");
            assert!(update["size"].as_u64().is_some_and(|tokens| tokens >= 100000));
            assert_eq!(update["_meta"]["harn"]["contextMeasurement"], "estimated");
            harn_vm::agent_sessions::canonical_history_boundaries(&store, root.path(), &session).await.unwrap();
            let source = update["_meta"]["harn"]["historySourceEventId"].as_str().expect("the fresh estimate must identify its actual appended message");
            let journal = store.read(&session, harn_session_store::ReadRange::default()).await.unwrap();
            let reply = journal.events.iter().find(|event| event.payload["transcript_event"]["id"] == source)
                .expect("context refresh must refer to the session's committed assistant source");
            assert_eq!(reply.payload["raw_message"]["role"], "assistant");
            assert_eq!(reply.payload["raw_message"]["content"], "measured reply");
            let before_append = updates.iter().rev().find(|update| update["_meta"]["harn"]["historySourceEventId"].is_null())
                .expect("the actual request context must also have been observed");
            assert!(update["used"].as_u64() > before_append["used"].as_u64(), "the canonical reply must refresh the resident estimate");
            let recorded = harn_vm::agent_session_spend::load_session_llm_cost_from_store(&store, &session).await.unwrap();
            if call == 3 {
                assert!(update["cost"].is_null(), "missing provider usage must not invent a price");
                assert_eq!(recorded.cost_usd, None);
                assert_eq!(update["_meta"]["harn"]["knownCostUsd"].as_f64(), Some(prior));
            } else {
                let cost = update["cost"]["amount"].as_f64().expect("reported usage has catalog pricing");
                assert_eq!(update["cost"]["currency"], "USD");
                assert_eq!(Some(cost), recorded.cost_usd);
                if call == 0 {assert_eq!(cost, 0.0);} else {assert!(cost > prior, "the next prompt must retain earlier actual call costs");}
                prior = cost;
            }
        }
        drop(tx);
        server.await.unwrap();
        http.abort();
        let _ = http.await;
    }).await;
}
