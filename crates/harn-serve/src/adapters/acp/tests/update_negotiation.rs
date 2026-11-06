use super::*;
use harn_vm::agent_events::{AgentEvent, AgentEventSink};

fn upstream_session_update_schema() -> serde_json::Value {
    let upstream: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/protocols/schemas/acp-1.26.0-upstream.schema.json"
    )))
    .unwrap();
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": "#/$defs/SessionNotification",
        "$defs": upstream["$defs"],
    })
}

#[test]
fn unconditional_update_vocabulary_is_upstream_and_extensions_require_exact_grants() {
    let schema = upstream_session_update_schema();
    let upstream = schema["$defs"]["SessionUpdate"]["oneOf"]
        .as_array()
        .unwrap();
    let capabilities = super::super::client_capabilities::ClientCapabilities::default();
    let mut standards = 0;
    for kind in ACP_SESSION_UPDATE_VARIANTS
        .iter()
        .chain(HARN_SESSION_UPDATE_EXTENSIONS.iter())
    {
        let is_upstream = upstream
            .iter()
            .any(|variant| variant["properties"]["sessionUpdate"]["const"] == *kind);
        assert_eq!(
            capabilities.supports_session_update(kind),
            is_upstream,
            "{kind}"
        );
        standards += usize::from(is_upstream);
    }
    assert!(
        standards >= 10,
        "the registry must exercise real standard updates"
    );
    for grant in [
        serde_json::json!(true),
        serde_json::json!("log"),
        serde_json::json!([42]),
    ] {
        let capabilities = super::super::client_capabilities::ClientCapabilities::from_initialize(
            &serde_json::json!({
                "clientCapabilities": {"_meta": {"harn": {"sessionUpdateExtensions": grant}}}
            }),
        );
        assert!(!capabilities.supports_session_update("log"));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn real_prompt_updates_obey_negotiated_extensions_and_validate_upstream() {
    harn_clock::test_support::with_local_tasks(async {
        let validator = jsonschema::draft202012::new(&upstream_session_update_schema()).unwrap();
        for extensions in [serde_json::json!([]), serde_json::json!(["log"])] {
            let root = tempfile::tempdir().unwrap();
            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(run_acp_channel_server(AcpServerConfig::new(None), request_rx, response_tx));
            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                "clientCapabilities":{"_meta":{"harn":{"sessionUpdateExtensions":extensions}}}
            }})).unwrap();
            assert!(recv_response_with_id(&mut response_rx, 1).await.get("result").is_some());
            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"session/new", "params":{
                "cwd":root.path(), "environmentPolicy":{"kind":"isolated"}
            }})).unwrap();
            let created = recv_response_with_id(&mut response_rx, 2).await;
            let session = created["result"]["sessionId"].as_str().unwrap();
            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt", "params":{
                "sessionId":session, "prompt":[{"type":"text", "text":concat!(
                    "harness.obs.log_warn(\"reached extension producer\")\n",
                    "progress(\"running\", \"unadvertised progress\")\n",
                    "harness.stdio.println(\"reached standard producer\")"
                )}]
            }})).unwrap();
            let mut standard = 0;
            let mut logs = 0;
            loop {
                let frame = recv_json(&mut response_rx).await;
                if frame["method"] == "host/capabilities" { reply_empty_result(&request_tx, &frame); }
                if frame["method"] == "session/update" {
                    let update = &frame["params"]["update"];
                    if update["sessionUpdate"] == "log" {
                        assert_eq!(extensions, serde_json::json!(["log"]));
                        assert_eq!(update["_meta"]["harn"]["message"], "reached extension producer");
                        logs += 1;
                    } else {
                        assert!(validator.is_valid(&frame["params"]), "strict client rejects {frame}");
                        if update["sessionUpdate"] == "agent_message_chunk" {
                            assert_eq!(update["content"]["text"], "reached standard producer\n");
                            let mut invalid = frame["params"].clone();
                            invalid["update"]["sessionUpdate"] = serde_json::json!("progress");
                            assert!(!validator.is_valid(&invalid), "schema must reject extension kinds");
                            standard += 1;
                        }
                    }
                }
                if frame["id"] == 3 && frame.get("method").is_none() {
                    assert_eq!(frame["result"]["stopReason"], "end_turn", "{frame}");
                    break;
                }
            }
            assert_eq!(standard, 1, "the real prompt must reach standard output");
            assert_eq!(logs, usize::from(extensions == serde_json::json!(["log"])));
            drop(request_tx);
            server.await.unwrap();
        }
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn replay_and_late_outputs_share_the_connections_current_extension_grants() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    let replay = AcpAgentEventSink::for_replay(server.output.clone());
    let event = AgentEvent::SkillActivated {
        session_id: "session".into(),
        skill_name: "example".into(),
        iteration: 0,
        reason: "matched".into(),
    };
    replay.handle_event(&event);
    assert!(rx.try_recv().is_err());
    server.handle_initialize(
        &serde_json::json!(1),
        &serde_json::json!({
            "clientCapabilities":{"_meta":{"harn":{"sessionUpdateExtensions":["skill_activated"]}}}
        }),
    );
    assert_eq!(recv_json(&mut rx).await["id"], 1);
    replay.handle_event(&event);
    let frame = recv_json(&mut rx).await;
    assert_eq!(
        frame["params"]["update"]["sessionUpdate"],
        "skill_activated"
    );
    assert_eq!(frame["params"]["update"]["_meta"]["harn"]["replayed"], true);
    server.handle_initialize(&serde_json::json!(2), &serde_json::json!({}));
    assert_eq!(recv_json(&mut rx).await["id"], 2);
    replay.handle_event(&event);
    assert!(
        rx.try_recv().is_err(),
        "an old output cannot retain revoked grants"
    );
}
