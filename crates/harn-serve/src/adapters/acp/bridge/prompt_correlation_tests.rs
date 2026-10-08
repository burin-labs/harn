use super::*;
use harn_vm::agent_events::{AgentEvent, AgentEventSink};
use harn_vm::bridge::HostBridge;

fn correlation(id: &str) -> AcpPromptCorrelation {
    serde_json::from_value(serde_json::json!({"messageId": id})).unwrap()
}

async fn receive(rx: &mut mpsc::UnboundedReceiver<String>) -> serde_json::Value {
    let line = harn_clock::test_support::within("prompt-scoped ACP frame", rx.recv())
        .await
        .expect("output remains open");
    serde_json::from_str(&line).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn same_session_delayed_calls_and_events_keep_their_prompt_owner() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let global = AcpOutput::Channel(tx);
    let old_output = global.for_prompt(correlation("old-prompt"));
    let current_output = global.for_prompt(correlation("current-prompt"));
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let host = |output: AcpOutput, first_id| {
        let bridge = HostBridge::from_parts_with_writer(
            pending.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |line| {
                output.write_line(line);
                Ok(())
            }),
            first_id,
        );
        bridge.set_session_id("same-session");
        bridge
    };
    let old = host(old_output.clone(), 1);
    let current = host(current_output.clone(), 100);
    for method in ["builtin_call", "skill/match", "session/request_permission"] {
        let old_call = old.call(method, serde_json::json!({"name": "old"}));
        tokio::pin!(old_call);
        let held = tokio::select! {
            frame = receive(&mut rx) => frame,
            result = &mut old_call => panic!("old callback settled before delivery: {result:?}"),
        };
        let current_call = current.call(method, serde_json::json!({"name": "current"}));
        tokio::pin!(current_call);
        let fresh = tokio::select! {
            frame = receive(&mut rx) => frame,
            result = &mut current_call => panic!("current callback settled before delivery: {result:?}"),
        };
        assert_eq!(fresh["params"]["sessionId"], "same-session");
        assert_eq!(
            fresh["params"]["promptCorrelation"]["messageId"],
            "current-prompt"
        );
        pending
            .lock()
            .await
            .remove(&fresh["id"].as_u64().unwrap())
            .unwrap()
            .send(serde_json::json!({"result": true}))
            .unwrap();
        assert_eq!(current_call.await.unwrap(), true);
        // Deliver the held frame only after the next prompt's positive call.
        assert_eq!(held["params"]["sessionId"], "same-session");
        assert_eq!(
            held["params"]["promptCorrelation"]["messageId"],
            "old-prompt"
        );
        pending
            .lock()
            .await
            .remove(&held["id"].as_u64().unwrap())
            .unwrap()
            .send(serde_json::json!({"error": {"message": "Inactive prompt"}}))
            .unwrap();
        assert!(old_call.await.is_err());
    }
    let acp = |output, first_id| AcpBridge {
        session_id: "same-session".into(),
        output,
        pending: pending.clone(),
        next_id_counter: AtomicU64::new(first_id),
        cancellation: SessionCancellation::default().prepare_prompt(),
        script_name: std::sync::Mutex::new(String::new()),
        assistant_state: std::sync::Mutex::new(VisibleTextState::default()),
    };
    let old_acp = acp(old_output.clone(), 200);
    let current_acp = acp(current_output.clone(), 300);
    let permission =
        serde_json::json!({"sessionId": "same-session", "toolCall": {}, "options": []});
    let old_permission = old_acp.call_client("session/request_permission", permission.clone());
    tokio::pin!(old_permission);
    let held = tokio::select! {
        frame = receive(&mut rx) => frame,
        result = &mut old_permission => panic!("old permission settled early: {result:?}"),
    };
    let current_permission = current_acp.call_client("session/request_permission", permission);
    tokio::pin!(current_permission);
    let fresh = tokio::select! {
        frame = receive(&mut rx) => frame,
        result = &mut current_permission => panic!("current permission settled early: {result:?}"),
    };
    assert_eq!(
        fresh["params"]["promptCorrelation"]["messageId"],
        "current-prompt"
    );
    pending
        .lock()
        .await
        .remove(&300)
        .unwrap()
        .send(serde_json::json!({"result": {"outcome": "selected"}}))
        .unwrap();
    assert_eq!(current_permission.await.unwrap()["outcome"], "selected");
    assert_eq!(
        held["params"]["promptCorrelation"]["messageId"],
        "old-prompt"
    );
    pending
        .lock()
        .await
        .remove(&200)
        .unwrap()
        .send(serde_json::json!({"error": {"message": "Inactive prompt"}}))
        .unwrap();
    assert!(old_permission.await.is_err());
    for (output, bridge, id) in [
        (old_output, &old, "old-prompt"),
        (current_output, &current, "current-prompt"),
    ] {
        bridge.notify(
            "harn.hitl.requested",
            serde_json::json!({"request_id": "approval"}),
        );
        bridge.send_output("visible output");
        let sink = events::AcpAgentEventSink::new(output);
        sink.handle_event(&AgentEvent::IterationStart {
            session_id: "same-session".into(),
            iteration: 1,
            provider: String::new(),
            model: String::new(),
        });
        for _ in 0..3 {
            assert_eq!(
                receive(&mut rx).await["params"]["promptCorrelation"]["messageId"],
                id
            );
        }
    }
    assert!(pending.lock().await.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn scoped_output_refuses_conflicts_and_preserves_global_replay_and_responses() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let global = AcpOutput::Channel(tx);
    let scoped = global.for_prompt(correlation("current"));
    let canary = serde_json::json!({"method": "builtin_call", "params": {
        "promptCorrelation": "credential-canary-must-not-escape"
    }})
    .to_string();
    scoped.write_line(&canary);
    let reason = correlate_prompt_frame(&canary, &correlation("current")).unwrap_err();
    assert_eq!(reason, "Invalid ACP prompt correlation");
    assert!(!reason.contains("credential-canary"));
    for supplied in [
        serde_json::json!({"messageId": "old"}),
        serde_json::json!({"messageId": ""}),
        serde_json::Value::Null,
    ] {
        scoped.write_line(
            &serde_json::json!({"jsonrpc": "2.0", "id": 7, "method": "builtin_call",
            "params": {"promptCorrelation": supplied}})
            .to_string(),
        );
    }
    scoped.write_line("not-json");
    scoped.write_line(r#"{"method":"builtin_call","params":null}"#);
    assert!(
        rx.try_recv().is_err(),
        "invalid prompt frames never escape untagged"
    );
    scoped.write_line(r#"{"jsonrpc":"2.0","id":8,"result":{"messageId":"result-id"}}"#);
    let response = receive(&mut rx).await;
    assert_eq!(response["id"], 8);
    assert_eq!(response["result"]["messageId"], "result-id");
    assert!(response.get("params").is_none());
    global.write_line(r#"{"method":"log","params":{"text":"global"}}"#);
    assert!(receive(&mut rx).await["params"]
        .get("promptCorrelation")
        .is_none());
    let replay = events::AcpAgentEventSink::for_replay(global);
    replay.handle_event(&AgentEvent::IterationStart {
        session_id: "same-session".into(),
        iteration: 1,
        provider: String::new(),
        model: String::new(),
    });
    let replayed = receive(&mut rx).await;
    assert_eq!(replayed["_harn"]["replayed"], true);
    assert!(replayed["params"].get("promptCorrelation").is_none());
    // Injected-message identity is distinct and must survive correlation.
    scoped.write_line(r#"{"method":"session/update","params":{"messageId":"injected","promptCorrelation":{"messageId":"current"}}}"#);
    let injected = receive(&mut rx).await;
    assert_eq!(injected["params"]["messageId"], "injected");
    let projected: AcpPromptCorrelation =
        serde_json::from_value(injected["params"]["promptCorrelation"].clone()).unwrap();
    assert_eq!(projected, correlation("current"));
}
