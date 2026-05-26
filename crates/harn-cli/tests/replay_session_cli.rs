use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

use harn_vm::event_log::{EventLog, LogEvent, SqliteEventLog, Topic};
use serde_json::json;

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).unwrap_or_else(|error| {
        panic!("stdout is not JSON: {error}\nstdout:\n{stdout}");
    })
}

fn normalized_run_json(run: &serde_json::Value) -> String {
    let mut run = run.clone();
    if let Some(events) = run.as_array_mut() {
        for event in events {
            if let Some(object) = event.as_object_mut() {
                object.remove("event_id");
                object.remove("occurred_at_ms");
            }
        }
    }
    serde_json::to_string(&run).expect("serialize normalized run")
}

fn unique_normalized_runs(parsed: &serde_json::Value) -> BTreeSet<String> {
    parsed["runs"]
        .as_array()
        .expect("runs should be an array")
        .iter()
        .map(normalized_run_json)
        .collect()
}

fn append_agent_event(
    log: &SqliteEventLog,
    topic: &Topic,
    session_id: &str,
    occurred_at_ms: i64,
    event: serde_json::Value,
) {
    let kind = event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .expect("agent event type")
        .to_string();
    let mut headers = BTreeMap::new();
    headers.insert("session_id".to_string(), session_id.to_string());
    let record = LogEvent {
        kind,
        payload: json!({
            "index_hint": occurred_at_ms,
            "session_id": session_id,
            "event": event,
        }),
        headers,
        occurred_at_ms,
    };
    futures::executor::block_on(log.append(topic, record)).expect("append event");
}

fn seed_session_db(path: &std::path::Path, session_id: &str) {
    let log = SqliteEventLog::open(path.to_path_buf(), 16).expect("open sqlite event log");
    let topic = Topic::new(format!(
        "observability.agent_events.{}",
        harn_vm::event_log::sanitize_topic_component(session_id)
    ))
    .expect("topic");
    append_agent_event(
        &log,
        &topic,
        session_id,
        1_770_000_000_000,
        json!({
            "type": "user_message",
            "session_id": session_id,
            "message_id": "msg_1",
            "content": [{"type": "text", "text": "Replay this session"}],
        }),
    );
    append_agent_event(
        &log,
        &topic,
        session_id,
        1_770_000_000_100,
        json!({
            "type": "iteration_start",
            "session_id": session_id,
            "iteration": 1,
            "provider": "mock",
            "model": "mock-model",
        }),
    );
    append_agent_event(
        &log,
        &topic,
        session_id,
        1_770_000_000_200,
        json!({
            "type": "agent_message_chunk",
            "session_id": session_id,
            "content": "done",
        }),
    );
    append_agent_event(
        &log,
        &topic,
        session_id,
        1_770_000_000_300,
        json!({
            "type": "session_closed",
            "session_id": session_id,
            "reason": "test complete",
            "status": "completed",
            "metadata": {},
        }),
    );
    futures::executor::block_on(log.flush()).expect("flush event log");
}

#[test]
fn replay_session_id_reads_events_db_and_compares_runs() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let db = temp.path().join("events.sqlite");
    let session_id = "session-2474";
    seed_session_db(&db, session_id);

    let out = Command::new(binary_path())
        .args([
            "replay",
            "--session-id",
            session_id,
            "--events-db",
            db.to_str().unwrap(),
            "--runs",
            "3",
            "--json",
        ])
        .output()
        .expect("spawn harn replay --session-id");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed = stdout_json(&out);
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["source"]["kind"], "event_log_session");
    assert_eq!(parsed["reports"].as_array().unwrap().len(), 3);
    assert_eq!(parsed["runs"].as_array().unwrap().len(), 3);
    assert_eq!(parsed["runs"][0].as_array().unwrap().len(), 4);
    assert_eq!(parsed["determinism"]["pass"], true);
    assert_eq!(parsed["reports"][0]["run_id"], session_id);
    assert_eq!(parsed["reports"][0]["transcript_event_count"], 4);
    assert_eq!(unique_normalized_runs(&parsed).len(), 1);
}

#[test]
fn replay_session_id_errors_when_session_absent() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let db = temp.path().join("events.sqlite");
    seed_session_db(&db, "session-present");

    let out = Command::new(binary_path())
        .args([
            "replay",
            "--session-id",
            "session-missing",
            "--events-db",
            db.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("spawn harn replay --session-id");
    assert!(!out.status.success(), "expected missing session to fail");
    let parsed = stdout_json(&out);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], "replay_load_failed");
    assert!(parsed["error"]["message"]
        .as_str()
        .unwrap_or("")
        .contains("session-missing"));
}

#[test]
fn replay_fixture_runs_use_oracle_allowlist() {
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/replay-oracle/fixtures/simple_trigger_local_handler.valid.json");
    let out = Command::new(binary_path())
        .args([
            "replay",
            "--fixture",
            fixture.to_str().unwrap(),
            "--runs",
            "3",
            "--json",
        ])
        .output()
        .expect("spawn harn replay --fixture --runs");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed = stdout_json(&out);
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["source"]["kind"], "replay_trace");
    assert_eq!(parsed["reports"].as_array().unwrap().len(), 3);
    assert_eq!(parsed["runs"].as_array().unwrap().len(), 3);
    assert_eq!(parsed["determinism"]["pass"], true);
    assert_eq!(unique_normalized_runs(&parsed).len(), 1);
}
