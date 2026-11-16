use super::*;
use harn_vm::agent_events::{AgentEvent, AgentEventSink};

pub(super) fn upstream_session_update_schema() -> serde_json::Value {
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

#[tokio::test(flavor = "current_thread")]
async fn real_warning_notice_requires_the_standard_session_grant() {
    harn_clock::test_support::with_local_tasks(async {
        let validator = jsonschema::draft202012::new(&upstream_session_update_schema()).unwrap();
        for (session_capabilities, expected) in [
            (serde_json::json!({}), 0),
            (serde_json::json!({"notices": false}), 0),
            (serde_json::json!({"notices": {}}), 1),
        ] {
            let root = tempfile::tempdir().unwrap();
            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(run_acp_channel_server(
                AcpServerConfig::new(None), request_rx, response_tx,
            ));
            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                "clientCapabilities":{"session":session_capabilities}
            }})).unwrap();
            assert!(recv_response_with_id(&mut response_rx, 1).await.get("result").is_some());
            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"session/new", "params":{
                "cwd":root.path(), "environmentPolicy":{"kind":"isolated"}
            }})).unwrap();
            let created = recv_response_with_id(&mut response_rx, 2).await;
            let session = created["result"]["sessionId"].as_str().unwrap();
            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt", "params":{
                "sessionId":session, "prompt":[{"type":"text", "text":concat!(
                    "harness.obs.log_warn(\"A real warning\", {diagnostic: \"private detail\"})\n",
                    "harness.obs.log_debug(\"debug only\")\n",
                    "harness.stdio.println(\"warning producer reached\")"
                )}]
            }})).unwrap();
            let mut notices = 0;
            let mut reached = false;
            loop {
                let frame = recv_json(&mut response_rx).await;
                if frame["method"] == "host/capabilities" { reply_empty_result(&request_tx, &frame); }
                if frame["method"] == "session/update" {
                    let params = &frame["params"];
                    assert!(validator.is_valid(params), "{frame}");
                    let update = &params["update"];
                    if update["sessionUpdate"] == "notice" {
                        assert_eq!(update["severity"], "warning");
                        assert_eq!(update["title"], "A real warning");
                        assert!(update.get("description").is_none());
                        notices += 1;
                    }
                    if update["sessionUpdate"] == "agent_message_chunk" {
                        assert_eq!(update["content"]["text"], "warning producer reached\n");
                        reached = true;
                    }
                }
                if frame["id"] == 3 && frame.get("method").is_none() {
                    assert_eq!(frame["result"]["stopReason"], "end_turn", "{frame}");
                    break;
                }
            }
            assert!(reached, "the actual prompt must execute");
            assert_eq!(notices, expected);
            drop(request_tx);
            server.await.unwrap();
        }
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn negotiated_compaction_projects_the_canonical_receipt_identity() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server.handle_initialize(
        &serde_json::json!(1),
        &serde_json::json!({"clientCapabilities": {
            "session": {"compaction": {}},
            "_meta": {"harn": {"sessionUpdateExtensions": ["transcript_compacted"]}}
        }}),
    );
    assert_eq!(recv_json(&mut rx).await["id"], 1);
    let receipt = harn_vm::orchestration::CompactionReceipt {
        schema_version: harn_vm::orchestration::COMPACTION_RECEIPT_SCHEMA_VERSION,
        receipt_id: "compaction-owned-17".into(),
        session_id: Some("session-owned".into()),
        estimated_tokens_before: 100,
        estimated_tokens_after: 40,
        ..Default::default()
    };
    let sink = AcpAgentEventSink::new(server.output.clone());
    sink.handle_event(&AgentEvent::TranscriptCompacted {
        session_id: "session-owned".into(),
        receipt: receipt.clone(),
    });
    let mut frames = Vec::new();
    while let Ok(line) = rx.try_recv() {
        frames.push(serde_json::from_str::<serde_json::Value>(&line).unwrap());
    }
    assert_eq!(
        frames
            .iter()
            .filter(|frame| frame["params"]["update"]["sessionUpdate"] == "transcript_compacted")
            .count(),
        1,
        "the known canonical receipt producer must be reached"
    );
    let stable = frames
        .iter()
        .find(|frame| frame["params"]["update"]["sessionUpdate"] == "compaction_update")
        .expect("a negotiated standard client must receive the compaction");
    let params = &stable["params"];
    assert_eq!(params["sessionId"], "session-owned");
    assert_eq!(params["update"]["compactionId"], "compaction-owned-17");
    assert_eq!(params["update"]["status"], "completed");
    let validator = jsonschema::draft202012::new(&upstream_session_update_schema()).unwrap();
    assert!(validator.is_valid(params), "{params}");
    let mut invalid = params.clone();
    invalid["update"]
        .as_object_mut()
        .unwrap()
        .remove("compactionId");
    assert!(!validator.is_valid(&invalid));
    invalid = params.clone();
    invalid["update"]["status"] = serde_json::json!(42);
    assert!(!validator.is_valid(&invalid));

    for capabilities in [
        serde_json::json!({}),
        serde_json::json!({"session":{"compaction":false}}),
        serde_json::json!({"_meta":{"harn":{"sessionUpdateExtensions":["compaction_update"]}}}),
    ] {
        server.handle_initialize(
            &serde_json::json!(2),
            &serde_json::json!({"clientCapabilities":capabilities}),
        );
        assert_eq!(recv_json(&mut rx).await["id"], 2);
        sink.handle_event(&AgentEvent::TranscriptCompacted {
            session_id: "session-owned".into(),
            receipt: receipt.clone(),
        });
        assert!(
            rx.try_recv().is_err(),
            "an old sink cannot retain revoked compaction grants"
        );
    }
    server.handle_initialize(
        &serde_json::json!(3),
        &serde_json::json!({"clientCapabilities":{"session":{"compaction":{}}}}),
    );
    assert_eq!(recv_json(&mut rx).await["id"], 3);
    let replay = AcpAgentEventSink::for_replay(server.output.clone());
    replay.handle_event(&AgentEvent::TranscriptCompacted {
        session_id: "session-owned".into(),
        receipt: receipt.clone(),
    });
    let replayed = recv_json(&mut rx).await;
    assert_eq!(
        replayed["params"]["update"]["compactionId"],
        "compaction-owned-17"
    );
    assert_eq!(replayed["params"]["update"]["status"], "completed");
    assert_eq!(
        replayed["params"]["update"]["_meta"]["harn"]["replayed"],
        true
    );
    let mut legacy = receipt;
    legacy.receipt_id.clear();
    replay.handle_event(&AgentEvent::TranscriptCompacted {
        session_id: "session-owned".into(),
        receipt: legacy,
    });
    assert!(
        rx.try_recv().is_err(),
        "legacy compaction cannot acquire an invented ID"
    );
}

#[test]
fn upstream_updates_obey_standard_capabilities_and_extensions_require_exact_grants() {
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
            is_upstream
                && !matches!(
                    *kind,
                    "notice" | "compaction_update" | "compaction_summary_chunk"
                ),
            "{kind}"
        );
        standards += usize::from(is_upstream);
    }
    assert!(
        standards >= 10,
        "the registry must exercise real standard updates"
    );
    let granted = super::super::client_capabilities::ClientCapabilities::from_initialize(
        &serde_json::json!({"clientCapabilities": {"session": {"notices": {}, "compaction": {}}}}),
    );
    for kind in ["notice", "compaction_update", "compaction_summary_chunk"] {
        assert!(granted.supports_session_update(kind), "{kind}");
        let extension_only = super::super::client_capabilities::ClientCapabilities::from_initialize(
            &serde_json::json!({"clientCapabilities": {"_meta": {"harn": {"sessionUpdateExtensions": [kind]}}}}),
        );
        assert!(
            !extension_only.supports_session_update(kind),
            "an extension grant cannot authorize {kind}"
        );
    }
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
async fn advertised_grants_deliver_user_message_history_identity() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    let replay = AcpAgentEventSink::for_replay(server.output.clone());
    let event = AgentEvent::UserMessage {
        session_id: "session".into(),
        message_id: "client-message".into(),
        content: vec![serde_json::json!({"type":"text", "text":"question"})],
        history_source_event_id: Some("history-event".into()),
    };

    server.handle_initialize(&serde_json::json!(1), &serde_json::json!({}));
    let initialized = recv_json(&mut rx).await;
    let advertised = initialized["result"]["agentCapabilities"]["_meta"]["harn"]
        ["sessionUpdateExtensions"]
        .as_array()
        .expect("the server advertises its gated updates")
        .clone();
    assert!(
        advertised.contains(&serde_json::json!("user_message")),
        "{advertised:?}"
    );
    replay.handle_event(&event);
    assert!(
        rx.try_recv().is_err(),
        "an ungranted nonstandard update must be suppressed"
    );

    server.handle_initialize(
        &serde_json::json!(2),
        &serde_json::json!({
            "clientCapabilities":{"_meta":{"harn":{"sessionUpdateExtensions":advertised}}}
        }),
    );
    assert_eq!(recv_json(&mut rx).await["id"], 2);
    replay.handle_event(&event);
    let frame = recv_json(&mut rx).await;
    assert_eq!(frame["params"]["update"]["sessionUpdate"], "user_message");
    assert_eq!(frame["params"]["update"]["messageId"], "client-message");
    assert_eq!(
        frame["params"]["update"]["historySourceEventId"],
        "history-event"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn real_prompt_updates_obey_negotiated_extensions_and_validate_upstream() {
    harn_clock::test_support::with_local_tasks(async {
        let validator = jsonschema::draft202012::new(&upstream_session_update_schema()).unwrap();
        let extension_schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"), "/../../conformance/protocols/schemas/acp-session-update.schema.json"
        ))).unwrap();
        let extension_validator = jsonschema::draft202012::new(&serde_json::json!({
            "$ref":"#/$defs/HarnExtensionUpdate", "$defs":extension_schema["$defs"]
        })).unwrap();
        for (capabilities, granted) in [(serde_json::json!({}), false), (serde_json::json!({
            "_meta":{"harn":{"sessionUpdateExtensions":["log", "progress"]}}
        }), true)] {
            let root = tempfile::tempdir().unwrap();
            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(run_acp_channel_server(AcpServerConfig::new(None), request_rx, response_tx));
            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"initialize", "params":{
                "clientCapabilities":capabilities
            }})).unwrap();
            assert!(recv_response_with_id(&mut response_rx, 1).await.get("result").is_some());
            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"session/new", "params":{
                "cwd":root.path(), "environmentPolicy":{"kind":"isolated"}
            }})).unwrap();
            let created = recv_response_with_id(&mut response_rx, 2).await;
            let session = created["result"]["sessionId"].as_str().unwrap();
            request_tx.send(serde_json::json!({"jsonrpc":"2.0", "id":3, "method":"session/prompt", "params":{
                "sessionId":session, "prompt":[{"type":"text", "text":concat!(
                    "harness.obs.log_warn(\"reached extension producer\", {source: \"probe\"})\n",
                    "harness.stdio.progress(\"probe\", \"full\", {current: 3, total: 10, data: {item: \"row-7\"}})\n",
                    "harness.stdio.progress(\"probe\", \"minimal\")\n",
                    "harness.stdio.println(\"reached standard producer\")"
                )}]
            }})).unwrap();
            let mut standard = 0;
            let mut logs = 0;
            let mut progress = Vec::new();
            loop {
                let frame = recv_json(&mut response_rx).await;
                if frame["method"] == "host/capabilities" { reply_empty_result(&request_tx, &frame); }
                if frame["method"] == "session/update" {
                    let update = &frame["params"]["update"];
                    if matches!(update["sessionUpdate"].as_str(), Some("log" | "progress")) {
                        assert!(granted, "unadvertised extension: {frame}");
                        assert!(extension_validator.is_valid(update), "{update}");
                        let meta = &update["_meta"]["harn"];
                        if update["sessionUpdate"] == "log" && meta["message"] == "reached extension producer" {
                            assert_eq!(meta["level"], "warn");
                            assert_eq!(meta["fields"]["source"], "probe");
                            logs += 1;
                        }
                        if update["sessionUpdate"] == "progress" && meta["phase"] == "probe" { progress.push(meta.clone()); }
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
            assert_eq!(logs, usize::from(granted));
            assert_eq!(progress.len(), if granted { 2 } else { 0 });
            if granted {
                assert_eq!(progress[0], serde_json::json!({"phase":"probe", "message":"full", "progress":3, "total":10, "data":{"item":"row-7"}}));
                assert_eq!(progress[1], serde_json::json!({"phase":"probe", "message":"minimal"}));
            }
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
