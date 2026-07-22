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

fn write_failing_run_record(path: &std::path::Path) {
    let run = json!({
        "_type": "run_record",
        "id": "run-failing-fixture",
        "workflow_id": "replay-fixture-exit",
        "workflow_name": "Replay fixture exit",
        "task": "Exercise human replay exit handling",
        "status": "failed",
        "started_at": "2026-05-26T00:00:00.000Z",
        "finished_at": "2026-05-26T00:00:01.000Z",
        "replay_fixture": {
            "_type": "replay_fixture",
            "id": "fixture-expects-completed",
            "source_run_id": "run-failing-fixture",
            "workflow_id": "replay-fixture-exit",
            "workflow_name": "Replay fixture exit",
            "created_at": "2026-05-26T00:00:01.000Z",
            "eval_kind": "replay",
            "expected_status": "completed",
            "stage_assertions": []
        }
    });
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&run).expect("serialize failing run record"),
    )
    .expect("write failing run record");
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
fn replay_human_runs_fail_when_embedded_fixture_fails() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let run_record = temp.path().join("run.json");
    write_failing_run_record(&run_record);

    let out = Command::new(binary_path())
        .args(["replay", run_record.to_str().unwrap(), "--runs", "2"])
        .output()
        .expect("spawn harn replay --runs");
    assert!(!out.status.success(), "expected replay fixture failure");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Embedded replay fixture: FAIL"));
    assert!(stdout.contains("run status mismatch"));
    assert!(stdout.contains("Determinism: PASS"));
}

#[test]
fn replay_counterfactual_reports_diverged_files_without_touching_disk() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let db = temp.path().join("events.sqlite");
    let session_id = "session-counterfactual";
    seed_session_db(&db, session_id);

    // A real workspace file the counterfactual plan will (virtually) edit.
    let target = temp.path().join("greeting.txt");
    let original = "hello world\nsecond line\n";
    std::fs::write(&target, original).expect("write target file");

    // The alternate plan: a `.harn` program whose final value is an edit
    // plan. The CLI runs it through `edit_dry_run` against a throw-away
    // staged-fs overlay — disk is never mutated.
    let plan = temp.path().join("plan.harn");
    std::fs::write(
        &plan,
        format!(
            r#"return [
  {{
    op: "safe_text_patch",
    path: "{}",
    old_text: "hello world",
    new_text: "hello counterfactual",
  }},
]
"#,
            target.to_str().unwrap(),
        ),
    )
    .expect("write counterfactual plan");

    let out = Command::new(binary_path())
        .args([
            "replay",
            "--session-id",
            session_id,
            "--events-db",
            db.to_str().unwrap(),
            "--counterfactual",
            plan.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("spawn harn replay --counterfactual");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed = stdout_json(&out);
    assert_eq!(parsed["ok"], true);
    let counterfactual = &parsed["data"]["counterfactual"];
    assert_eq!(counterfactual["result"], "ok");
    assert_eq!(counterfactual["files_touched"], 1);
    let diverged = counterfactual["diverged"]
        .as_array()
        .expect("diverged should be an array");
    assert_eq!(diverged.len(), 1);
    assert_eq!(
        diverged[0]["path"].as_str().unwrap(),
        target.to_str().unwrap()
    );
    assert_eq!(diverged[0]["status"], "modified");
    assert_eq!(diverged[0]["lines_added"], 1);
    assert_eq!(diverged[0]["lines_removed"], 1);

    // The dry-run never touches disk — the file is byte-identical.
    let on_disk = std::fs::read_to_string(&target).expect("read target after replay");
    assert_eq!(on_disk, original);
}

#[test]
fn replay_counterfactual_human_lists_diverged_files() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let db = temp.path().join("events.sqlite");
    let session_id = "session-counterfactual-human";
    seed_session_db(&db, session_id);

    let target = temp.path().join("notes.txt");
    std::fs::write(&target, "alpha\nbeta\n").expect("write target file");

    let plan = temp.path().join("plan.harn");
    std::fs::write(
        &plan,
        format!(
            r#"return [
  {{op: "safe_text_patch", path: "{}", old_text: "alpha", new_text: "alpha-prime"}},
]
"#,
            target.to_str().unwrap(),
        ),
    )
    .expect("write plan");

    let out = Command::new(binary_path())
        .args([
            "replay",
            "--session-id",
            session_id,
            "--events-db",
            db.to_str().unwrap(),
            "--counterfactual",
            plan.to_str().unwrap(),
        ])
        .output()
        .expect("spawn harn replay --counterfactual (human)");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Counterfactual:"),
        "human output should announce the counterfactual:\n{stdout}"
    );
    assert!(
        stdout.contains("would touch 1 file(s)"),
        "human output should summarize the divergent file set:\n{stdout}"
    );
    assert!(
        stdout.contains(target.to_str().unwrap()),
        "human output should name the diverged file:\n{stdout}"
    );
}

#[test]
fn replay_counterfactual_accepts_plan_that_returns_dry_run_result() {
    // The alternate contract: the plan calls `edit_dry_run` itself and
    // returns its result dict. The CLI reads divergence off the same
    // `per_file_unified_diff` shape rather than re-running the dry-run.
    let temp = tempfile::TempDir::new().expect("tempdir");
    let db = temp.path().join("events.sqlite");
    let session_id = "session-counterfactual-dict";
    seed_session_db(&db, session_id);

    let target = temp.path().join("config.txt");
    std::fs::write(&target, "mode = off\n").expect("write target file");

    let plan = temp.path().join("plan.harn");
    std::fs::write(
        &plan,
        format!(
            r#"import {{ edit_dry_run }} from "std/edit"
return edit_dry_run({{plan: [
  {{op: "safe_text_patch", path: "{}", old_text: "mode = off", new_text: "mode = on"}},
]}})
"#,
            target.to_str().unwrap(),
        ),
    )
    .expect("write plan");

    let out = Command::new(binary_path())
        .args([
            "replay",
            "--session-id",
            session_id,
            "--events-db",
            db.to_str().unwrap(),
            "--counterfactual",
            plan.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("spawn harn replay --counterfactual (dict)");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed = stdout_json(&out);
    let counterfactual = &parsed["data"]["counterfactual"];
    assert_eq!(counterfactual["result"], "ok");
    assert_eq!(counterfactual["files_touched"], 1);
    let diverged = counterfactual["diverged"].as_array().unwrap();
    assert_eq!(diverged.len(), 1);
    assert_eq!(diverged[0]["status"], "modified");
}

#[test]
fn replay_counterfactual_chains_plans_cumulatively() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let db = temp.path().join("events.sqlite");
    let session_id = "session-counterfactual-chain";
    seed_session_db(&db, session_id);

    let target = temp.path().join("chain.txt");
    let original = "alpha\n";
    std::fs::write(&target, original).expect("write target file");

    let first = temp.path().join("first.harn");
    std::fs::write(
        &first,
        format!(
            r#"return [
  {{op: "safe_text_patch", path: "{}", old_text: "alpha", new_text: "beta"}},
]
"#,
            target.to_str().unwrap(),
        ),
    )
    .expect("write first plan");
    let second = temp.path().join("second.harn");
    std::fs::write(
        &second,
        format!(
            r#"return [
  {{op: "safe_text_patch", path: "{}", old_text: "beta", new_text: "gamma"}},
]
"#,
            target.to_str().unwrap(),
        ),
    )
    .expect("write second plan");

    let out = Command::new(binary_path())
        .args([
            "replay",
            "--session-id",
            session_id,
            "--events-db",
            db.to_str().unwrap(),
            "--counterfactual",
            first.to_str().unwrap(),
            "--counterfactual",
            second.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("spawn harn replay --counterfactual chain");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed = stdout_json(&out);
    let counterfactual = &parsed["data"]["counterfactual"];
    assert_eq!(counterfactual["result"], "ok");
    assert_eq!(counterfactual["step_count"], 2);
    assert_eq!(counterfactual["ops_applied"], 2);
    assert_eq!(counterfactual["files_touched"], 1);
    assert_eq!(
        counterfactual["plan_paths"].as_array().unwrap().len(),
        2,
        "plan paths should preserve chain order"
    );
    let diverged = counterfactual["diverged"].as_array().unwrap();
    assert_eq!(diverged.len(), 1);
    assert_eq!(diverged[0]["status"], "modified");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
}

#[test]
fn replay_counterfactual_plan_side_effects_are_isolated() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let db = temp.path().join("events.sqlite");
    let session_id = "session-counterfactual-isolated";
    seed_session_db(&db, session_id);

    let target = temp.path().join("isolated.txt");
    let original = "safe\n";
    std::fs::write(&target, original).expect("write target file");

    let plan = temp.path().join("plan.harn");
    std::fs::write(
        &plan,
        format!(
            r#"write_file("{}", "mutated before return\n")
return [
  {{op: "safe_text_patch", path: "{}", old_text: "safe", new_text: "preview"}},
]
"#,
            target.to_str().unwrap(),
            target.to_str().unwrap(),
        ),
    )
    .expect("write plan");

    let out = Command::new(binary_path())
        .args([
            "replay",
            "--session-id",
            session_id,
            "--events-db",
            db.to_str().unwrap(),
            "--counterfactual",
            plan.to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("spawn harn replay --counterfactual isolated");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed = stdout_json(&out);
    assert_eq!(parsed["data"]["counterfactual"]["result"], "ok");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
    let state_dir = temp.path().join(".harn").join("state").join("staged");
    if state_dir.exists() {
        let leaked_counterfactual_sessions = std::fs::read_dir(&state_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("harn-counterfactual-")
            })
            .count();
        assert_eq!(leaked_counterfactual_sessions, 0);
    }
}

#[test]
fn replay_counterfactual_rejects_missing_plan() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let db = temp.path().join("events.sqlite");
    let session_id = "session-counterfactual-missing";
    seed_session_db(&db, session_id);

    let out = Command::new(binary_path())
        .args([
            "replay",
            "--session-id",
            session_id,
            "--events-db",
            db.to_str().unwrap(),
            "--counterfactual",
            temp.path().join("does-not-exist.harn").to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("spawn harn replay --counterfactual missing");
    assert!(!out.status.success(), "expected missing plan to fail");
    let parsed = stdout_json(&out);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], "replay_counterfactual_failed");
}

#[test]
fn replay_counterfactual_does_not_evaluate_when_cutoff_is_invalid() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let db = temp.path().join("events.sqlite");
    let session_id = "session-counterfactual-bad-cutoff";
    seed_session_db(&db, session_id);

    let out = Command::new(binary_path())
        .args([
            "replay",
            "--session-id",
            session_id,
            "--events-db",
            db.to_str().unwrap(),
            "--at",
            "0",
            "--counterfactual",
            temp.path().join("does-not-exist.harn").to_str().unwrap(),
            "--json",
        ])
        .output()
        .expect("spawn harn replay --counterfactual invalid cutoff");
    assert!(!out.status.success(), "expected invalid cutoff to fail");
    let parsed = stdout_json(&out);
    assert_eq!(parsed["error"]["code"], "replay_load_failed");
    assert!(parsed["error"]["message"]
        .as_str()
        .unwrap_or("")
        .contains("event id 0"));
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
