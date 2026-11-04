use super::event_log_barrier::ResetActiveEventLog;
use super::*;
use harn_session_store::{AppendEvent, SessionEventKind, SessionStore, SessionWriteLease};

#[tokio::test(flavor = "current_thread")]
async fn continuation_does_not_reissue_a_text_tool_call_id() {
    continuation_tool_call_ids("text").await;
}

#[tokio::test(flavor = "current_thread")]
async fn continuation_does_not_reissue_a_native_tool_call_id() {
    continuation_tool_call_ids("native").await;
}

async fn continuation_tool_call_ids(format: &str) {
    let _reset = ResetActiveEventLog;
    harn_vm::reset_thread_local_state();
    tokio::task::LocalSet::new().run_until(async {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().canonicalize().unwrap();
        let pipeline = project.join("text.harn");
        let source = r#"import { agent_loop } from "std/agent/loop"
pipeline default(harness: Harness) {
  harness.llm.mock_clear()
  harness.llm.mock_enqueue(MOCK_TOOL_RESPONSE)
  harness.llm.mock_enqueue({text: "tool continuation reply"})
  const tools = tool_define(tool_registry(), "noop", "Read a fixture", {
    handler: { args -> "actual tool result" }, parameters: {}, returns: {type: "string"},
    annotations: {kind: "read"},
  })
  agent_loop(harness, prompt, nil, {provider: "mock", tools: tools, tool_format: "TOOL_FORMAT"})
  assert_eq(len(harness.llm.mock_calls()), 2)
}
"#;
        let mock_response = if format == "native" {
            r#"{tool_calls: [{name: "noop", arguments: {}}]}"#
        } else {
            r#"{text: "<tool_call>{\"name\":\"noop\",\"arguments\":{}}</tool_call>"}"#
        };
        let source = source.replace("MOCK_TOOL_RESPONSE", mock_response).replace("TOOL_FORMAT", format);
        std::fs::write(&pipeline, source).unwrap();
        let config = AcpServerConfig::for_pipeline(pipeline.display().to_string())
            .with_launcher_environment(harn_vm::security::LauncherEnvironment::from_snapshot(Default::default()));
        let (tx, mut rx, server, session) = start_acp_code_session_with_config(config, serde_json::json!(project)).await;
        let mut continuation = serde_json::Value::Null;
        let mut call_ids = Vec::new();
        for turn in 0..3 {
            if turn > 0 {
                let mut params = serde_json::json!({
                    "sessionId":session, "cwd":project,
                    "environmentPolicy":{"kind":"isolated"},
                });
                if turn == 1 { params["continuation"] = continuation.clone(); }
                let (loaded, _) = request(&tx, &mut rx, 20, "session/load", params).await;
                assert_eq!(loaded["replayed"].as_array().unwrap().is_empty(), turn == 1);
                request(&tx, &mut rx, 21, "session/set_mode", serde_json::json!({"sessionId":session, "modeId":"code"})).await;
            }
            let (result, events) = request(&tx, &mut rx, 10 * turn + 3, "session/prompt", serde_json::json!({
                "sessionId":session, "prompt":[{"type":"text", "text":"Run the tool then reply"}],
            })).await;
            assert_eq!(result["stopReason"], "end_turn");
            let ids: Vec<_> = events.iter().filter_map(|event| {
                let update = &event["params"]["update"];
                (update["sessionUpdate"] == "tool_call").then(|| update["toolCallId"].as_str().unwrap().to_owned())
            }).collect();
            assert_eq!(ids.len(), 1, "the actual {format} tool must fire: {events:?}");
            call_ids.extend(ids);
            let (closed, _) = request(&tx, &mut rx, 10 * turn + 4, "session/close", serde_json::json!({"sessionId":session})).await;
            continuation = closed["continuation"].clone();
            assert!(continuation.is_string());
        }
        assert_ne!(call_ids[0], call_ids[1], "a restored tool result must not answer a new call");
        assert_ne!(call_ids[1], call_ids[2], "cold restore must retain the tool-call high-water mark");
        assert_ne!(call_ids[0], call_ids[2]);
        drop(tx);
        server.await.unwrap();
    }).await;
}

async fn request(
    tx: &mpsc::UnboundedSender<serde_json::Value>,
    rx: &mut mpsc::UnboundedReceiver<String>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> (serde_json::Value, Vec<serde_json::Value>) {
    let (response, events) = request_response(tx, rx, id, method, params).await;
    assert!(response.get("error").is_none(), "{method}: {response}");
    (response["result"].clone(), events)
}

async fn request_response(
    tx: &mpsc::UnboundedSender<serde_json::Value>,
    rx: &mut mpsc::UnboundedReceiver<String>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> (serde_json::Value, Vec<serde_json::Value>) {
    tx.send(serde_json::json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
        .unwrap();
    let mut events = Vec::new();
    loop {
        let message = recv_json(rx).await;
        if message["method"] == "host/capabilities" {
            tx.send(serde_json::json!({"jsonrpc":"2.0", "id":message["id"], "result":{}}))
                .unwrap();
        } else if message["id"] == id {
            return (message, events);
        } else {
            events.push(message);
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn closed_session_continuation_preserves_history_and_invalidates_after_an_external_write() {
    let _reset = ResetActiveEventLog;
    harn_vm::reset_thread_local_state();
    tokio::task::LocalSet::new().run_until(async {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().canonicalize().unwrap();
        let pipeline = project.join("reply.harn");
        std::fs::write(&pipeline, r#"import { agent_loop } from "std/agent/loop"
pipeline default(harness: Harness) {
  if prompt == "1" {
    assert_eq(len(harness.secrets.inputs()), 1)
  } else {
    assert_eq(len(harness.secrets.inputs()), 0)
  }
  harness.llm.mock_clear()
  harness.llm.mock_enqueue({text: "continuation reply"})
  agent_loop(harness, prompt, nil, {provider: "mock"})
  const calls = harness.llm.mock_calls()
  assert_eq(len(calls), 1)
  const users = calls[0].messages.filter({ message -> message.role == "user" }).to_list()
  assert_eq(len(users.filter({ message -> message.content == "1" }).to_list()), 1)
  if prompt == "2" || prompt == "3" {
    assert_eq(len(users.filter({ message -> message.content == "2" }).to_list()), 1)
  }
  if prompt == "3" {
    assert_eq(len(users.filter({ message -> message.content == "3" }).to_list()), 1)
  }
  if prompt == "4" {
    assert_eq(len(calls[0].messages.filter({ message -> message.content == "external history" }).to_list()), 1)
  }
}
"#).unwrap();
        let config = AcpServerConfig::for_pipeline(pipeline.display().to_string())
            .with_launcher_environment(harn_vm::security::LauncherEnvironment::from_snapshot(Default::default()));
        let (tx, mut rx, server, session_id) = start_acp_code_session_with_config(config, serde_json::json!(project)).await;
        let (added, _) = request(&tx, &mut rx, 5, "session/inputs", serde_json::json!({
            "sessionId":session_id, "command":{"kind":"put", "spec":{"name":"CONTINUATION_KEY"}, "value":"synthetic-continuation-canary"},
        })).await;
        assert_eq!(added["input"]["name"], "CONTINUATION_KEY");
        let mut continuation = serde_json::Value::Null;
        for turn in 1..=3 {
            if turn > 1 {
                let (loaded, events) = request(&tx, &mut rx, 10 * turn, "session/load", serde_json::json!({
                    "sessionId":session_id, "cwd":project, "continuation":continuation,
                    "environmentPolicy":{"kind":"isolated"},
                })).await;
                assert_eq!(loaded["replayed"], serde_json::json!([]));
                assert!(!events.iter().any(|event| event["params"]["update"]["_meta"]["harn"]["replayed"] == true));
                let (inputs, _) = request(&tx, &mut rx, 10 * turn + 4, "session/inputs", serde_json::json!({
                    "sessionId":session_id, "command":{"kind":"list"},
                })).await;
                assert_eq!(inputs["inputs"], serde_json::json!([]), "closed input grants cannot reappear");
                request(&tx, &mut rx, 10 * turn + 5, "session/set_mode", serde_json::json!({
                    "sessionId":session_id, "modeId":"code",
                })).await;
            }
            let (prompt, events) = request(&tx, &mut rx, 10 * turn + 1, "session/prompt", serde_json::json!({
                "sessionId":session_id, "prompt":[{"type":"text", "text":turn.to_string()}],
            })).await;
            assert_eq!(prompt["stopReason"], "end_turn");
            assert!(events.iter().any(|event| event.to_string().contains("continuation reply")), "actual model reply must be delivered");
            assert!(!events.iter().any(|event| event.to_string().contains("synthetic-continuation-canary")));
            let (closed, _) = request(&tx, &mut rx, 10 * turn + 2, "session/close", serde_json::json!({"sessionId":session_id})).await;
            continuation = closed["continuation"].clone();
            assert!(continuation.as_str().is_some_and(|token| !token.is_empty()), "closed turn must issue its continuation: {closed}");
            tx.send(serde_json::json!({"jsonrpc":"2.0", "id":10 * turn + 3, "method":"session/resume",
                "params":{"sessionId":session_id}})).unwrap();
            let retired = recv_response_with_id(&mut rx, 10 * turn + 3).await;
            assert!(retired.get("error").is_some(), "close must retire the runnable session: {retired}");
            let _lease = SessionWriteLease::try_acquire(&project.join(".harn"), &session_id).expect("close releases the canonical writer");
        }
        let (loaded, _) = request(&tx, &mut rx, 40, "session/load", serde_json::json!({
            "sessionId":session_id, "cwd":project, "continuation":continuation,
            "environmentPolicy":{"kind":"isolated"},
        })).await;
        assert_eq!(loaded["replayed"], serde_json::json!([]));
        request(&tx, &mut rx, 45, "session/set_mode", serde_json::json!({"sessionId":session_id, "modeId":"code"})).await;
        let store = harn_vm::open_canonical_store(&project).unwrap();
        {
            let _lease = SessionWriteLease::try_acquire(&project.join(".harn"), &session_id).unwrap();
            store.append(&session_id, AppendEvent::new(SessionEventKind::Message, serde_json::json!({
                "transcript_event":{"kind":"message", "role":"assistant", "visibility":"public", "text":"external history"},
                "raw_message":{"role":"assistant", "content":"external history"},
            }))).await.unwrap();
        }
        let (refused, events) = request_response(&tx, &mut rx, 41, "session/prompt", serde_json::json!({
            "sessionId":session_id, "prompt":[{"type":"text", "text":"4"}],
        })).await;
        assert!(refused["error"]["message"].as_str().unwrap_or_default().contains("close and reload"), "{refused}");
        assert!(!events.iter().any(|event| event.to_string().contains("continuation reply")), "a stale continuation cannot reach the model");
        request(&tx, &mut rx, 42, "session/close", serde_json::json!({"sessionId":session_id})).await;
        let (loaded, replay) = request(&tx, &mut rx, 43, "session/load", serde_json::json!({
            "sessionId":session_id, "cwd":project, "continuation":continuation,
            "environmentPolicy":{"kind":"isolated"},
        })).await;
        assert!(!loaded["replayed"].as_array().unwrap().is_empty());
        assert!(replay.iter().any(|event| event.to_string().contains("external history")), "changed history must be delivered");
        request(&tx, &mut rx, 44, "session/set_mode", serde_json::json!({"sessionId":session_id, "modeId":"code"})).await;
        let (result, _) = request(&tx, &mut rx, 46, "session/prompt", serde_json::json!({
            "sessionId":session_id, "prompt":[{"type":"text", "text":"4"}],
        })).await;
        assert_eq!(result["stopReason"], "end_turn", "cold replay must also restore the model's history");
        drop(tx);
        server.await.unwrap();
    }).await;
}
