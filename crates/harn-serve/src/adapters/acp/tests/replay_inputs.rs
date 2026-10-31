use super::*;
use harn_vm::run_tape::{start_recording, RecordingLevel, TAPE_FILE_NAME};
use harn_vm::testbench::tape::EventTape;
use serde_json::json;

#[tokio::test(flavor = "current_thread")]
async fn recorded_client_inputs_reconstruct_completed_turns_and_refuse_private_controls() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("record.harn");
    std::fs::write(
        &source,
        "fn main(harness: Harness) { harness.stdio.println(\"reached-input-recording\") }",
    )
    .unwrap();
    let tape_dir = root.path().join("tape");
    let recording = start_recording(&tape_dir, RecordingLevel::Full, None)
        .unwrap()
        .unwrap();
    tokio::task::LocalSet::new().run_until(async {
        let (tx, mut rx, server, session) = start_acp_channel_session_with_config(
            AcpServerConfig::new(Some(source.to_string_lossy().into_owned())), json!(root.path()),
        ).await;
        for id in [2, 3] {
            tx.send(json!({"jsonrpc":"2.0", "id":id, "method":"session/prompt", "params":{
                "sessionId":session, "prompt":[{"type":"text", "text":format!("turn-{id}")}]
            }})).unwrap();
            let mut reached = false;
            loop {
                let frame = recv_json(&mut rx).await;
                if frame["method"] == "host/capabilities" { reply_empty_result(&tx, &frame); }
                if frame["params"]["update"]["content"]["text"].as_str().is_some_and(|text| text.contains("reached-input-recording")) { reached = true; }
                if frame["id"] == id && frame.get("method").is_none() {
                    assert!(frame.get("result").is_some(), "{frame}");
                    break;
                }
            }
            assert!(reached, "the recorded prompt must execute the real pipeline");
        }
        let tape = EventTape::load(&tape_dir.join(TAPE_FILE_NAME)).unwrap();
        let inputs = super::super::replay_inputs::replay_requests(&tape).unwrap();
        assert_eq!(inputs.len(), 4);
        let mut sessions = std::collections::BTreeMap::new();
        assert_eq!(inputs[1].params("/isolated", &sessions).unwrap()["cwd"], "/isolated");
        assert_eq!(inputs[1].created_session(), Some(session.as_str()));
        assert!(inputs[3].params("/isolated", &sessions).is_err());
        sessions.insert(session.clone(), "fresh-session".into());
        let replayed = inputs[3].params("/isolated", &sessions).unwrap();
        assert_eq!(replayed["sessionId"], "fresh-session");
        assert_eq!(replayed["prompt"][0]["text"], "turn-3");

        tx.send(json!({"jsonrpc":"2.0", "id":10, "method":"session/new", "params":{
            "cwd":root.path(), "environmentPolicy":{"kind":"isolated", "grants":[]}
        }})).unwrap();
        let second = recv_response_with_id(&mut rx, 10).await["result"]["sessionId"].as_str().unwrap().to_owned();
        tx.send(json!({"jsonrpc":"2.0", "id":11, "method":"session/prompt", "params":{
            "sessionId":second, "prompt":[{"type":"text", "text":"second session"}]
        }})).unwrap();
        let mut reached = false;
        loop {
            let frame = recv_json(&mut rx).await;
            if frame["method"] == "host/capabilities" { reply_empty_result(&tx, &frame); }
            if frame["params"]["update"]["content"]["text"].as_str().is_some_and(|text| text.contains("reached-input-recording")) { reached = true; }
            if frame["id"] == 11 && frame.get("method").is_none() {
                assert!(frame.get("result").is_some(), "{frame}");
                break;
            }
        }
        assert!(reached);
        let tape = EventTape::load(&tape_dir.join(TAPE_FILE_NAME)).unwrap();
        let inputs = super::super::replay_inputs::replay_requests(&tape).unwrap();
        assert_eq!(inputs.len(), 6);
        assert_eq!(inputs[4].created_session(), Some(second.as_str()));
        assert!(inputs[5].params("/isolated", &sessions).is_err());
        sessions.insert(second, "second-replay".into());
        assert_eq!(inputs[5].params("/isolated", &sessions).unwrap()["sessionId"], "second-replay");
        assert_eq!(inputs[3].params("/isolated", &sessions).unwrap()["sessionId"], "fresh-session");

        tx.send(json!({"jsonrpc":"2.0", "id":4, "method":"session/inputs", "params":{
            "sessionId":session, "command":{"kind":"put", "spec":{"name":"KEY"}, "value":"private-input-canary"}
        }})).unwrap();
        assert!(recv_response_with_id(&mut rx, 4).await.get("result").is_some());
        tx.send(json!({"jsonrpc":"2.0", "id":5, "method":"session/prompt", "params":{
            "sessionId":session, "prompt":[{"type":"text", "text":"private-input-canary"}]
        }})).unwrap();
        loop {
            let frame = recv_json(&mut rx).await;
            if frame["method"] == "host/capabilities" {
                reply_empty_result(&tx, &frame);
            } else if frame["id"] == 5 && frame.get("method").is_none() {
                assert!(frame.get("result").is_some(), "{frame}");
                break;
            }
        }
        drop(tx);
        server.await.unwrap();
    }).await;
    assert!(recording.error().is_none());
    drop(recording);
    let tape = EventTape::load(&tape_dir.join(TAPE_FILE_NAME)).unwrap();
    assert!(super::super::replay_inputs::replay_requests(&tape)
        .unwrap_err()
        .contains("without a replay schedule"));
    for record in &tape.records {
        if let harn_vm::testbench::tape::TapeRecordKind::SessionControl { event } = &record.kind {
            let bytes = tape.resolve_payload(event).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("private-input-canary"));
        }
    }
    let empty = EventTape::new(tape.header.clone());
    assert!(super::super::replay_inputs::replay_requests(&empty)
        .unwrap_err()
        .contains("no complete"));
}
