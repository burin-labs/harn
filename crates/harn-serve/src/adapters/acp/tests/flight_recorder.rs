use super::*;
use harn_vm::flight_recorder::{FlightRecorderOptions, FlightRecording};

#[derive(Default)]
struct RecordOneSession(Mutex<Option<String>>);

#[async_trait::async_trait(?Send)]
impl AcpRuntimeConfigurator for RecordOneSession {
    fn flight_recorder(&self, session_id: &str) -> Option<FlightRecorderOptions> {
        let mut selected = self.0.lock().unwrap();
        let selected = selected.get_or_insert_with(|| session_id.to_string());
        Some(FlightRecorderOptions {
            enabled: selected == session_id,
            max_events: 1,
            ..Default::default()
        })
    }
}

fn recordings(root: &Path) -> Vec<serde_json::Value> {
    let path = harn_vm::runtime_paths::run_root(root).join("flight-recordings");
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!("could not read recordings: {error}"),
    };
    entries
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap()
        })
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn session_selection_records_each_executed_prompt_with_complete_intern_tables() {
    harn_clock::test_support::with_local_tasks(async {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("record.harn");
        std::fs::write(&source, r#"fn main(harness: Harness) -> nil {
  harness.stdio.println("flight-probe")
  if prompt == "fail" { throw "flight-failure" }
}"#).unwrap();
        let config = AcpServerConfig::for_pipeline(source.to_string_lossy().into_owned())
            .with_flight_recorder(FlightRecorderOptions::default())
            .with_runtime_configurator(Arc::new(RecordOneSession::default()));
        let (tx, mut rx, server, first) = start_acp_channel_session_with_config(config, serde_json::json!(root.path())).await;
        assert!(recordings(root.path()).is_empty());
        for (id, text) in [(2, "pass"), (3, "fail")] {
            tx.send(serde_json::json!({"jsonrpc":"2.0", "id":id, "method":"session/prompt", "params":{
                "sessionId":first, "messageId":format!("prompt-{id}"), "prompt":[{"type":"text", "text":text}]
            }})).unwrap();
            let mut reached = false;
            loop {
                let frame = recv_json(&mut rx).await;
                if frame["method"] == "host/capabilities" { reply_empty_result(&tx, &frame); }
                reached |= frame["params"]["update"]["content"]["text"].as_str().is_some_and(|text| text.contains("flight-probe"));
                if frame["id"] == id && frame.get("method").is_none() {
                    assert_eq!(frame.get("error").is_some(), text == "fail", "{frame}");
                    break;
                }
            }
            assert!(reached, "the actual entry pipeline must run");
            let files = recordings(root.path());
            assert_eq!(files.len(), (id - 1) as usize);
            let file = files.iter().find(|file| file["context"]["prompt_id"] == format!("prompt-{id}")).unwrap();
            assert_eq!(file["context"]["session_id"], first);
            let recording: FlightRecording = serde_json::from_value(file.clone()).unwrap();
            assert_eq!(recording.events.len(), 1);
            assert!(recording.dropped_events > 0);
            assert!(recording.sources.iter().any(|path| path.ends_with("record.harn")));
            assert!(recording.functions.iter().any(|name| name.contains("main")));
            assert!(recording.terminal.is_some());
            assert!(!serde_json::to_string(file).unwrap().contains("flight-probe"), "runtime values must stay omitted");
        }
        tx.send(serde_json::json!({"jsonrpc":"2.0", "id":4, "method":"session/new", "params":{
            "cwd":root.path(), "environmentPolicy":{"kind":"isolated"}
        }})).unwrap();
        let second = recv_response_with_id(&mut rx, 4).await["result"]["sessionId"].as_str().unwrap().to_owned();
        assert_ne!(first, second);
        tx.send(serde_json::json!({"jsonrpc":"2.0", "id":5, "method":"session/prompt", "params":{
            "sessionId":second, "prompt":[{"type":"text", "text":"pass"}]
        }})).unwrap();
        let mut reached = false;
        loop {
            let frame = recv_json(&mut rx).await;
            if frame["method"] == "host/capabilities" { reply_empty_result(&tx, &frame); }
            reached |= frame["params"]["update"]["content"]["text"].as_str().is_some_and(|text| text.contains("flight-probe"));
            if frame["id"] == 5 && frame.get("method").is_none() {
                assert_eq!(frame["result"]["stopReason"], "end_turn", "{frame}");
                break;
            }
        }
        assert!(reached, "the disabled session must actually execute");
        assert_eq!(recordings(root.path()).len(), 2, "disabled session writes no recording");
        drop(tx);
        server.await.unwrap();
    }).await;
}
