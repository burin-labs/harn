//! Record imports reject malformed private payloads without quoting source data.

use std::path::Path;

use serde_json::json;

use super::*;
use crate::value::VmError;

fn assert_private_parse_error(error: VmError, label: &str) {
    assert_eq!(
        error.to_string(),
        format!("Runtime error: {label} parse error: payload does not match expected schema")
    );
}

#[test]
fn public_record_file_loaders_reject_private_values_and_preserve_valid_records() {
    type Loader = fn(&Path) -> Result<(), VmError>;
    let cases: [(&str, &str, &str, &str, Loader); 4] = [
        (
            "run.json",
            r#"{"tool_recordings":[{"duration_ms":"private-run-canary"}]}"#,
            r#"{"id":"valid-run","status":"success"}"#,
            "run record",
            |path| load_run_record(path).map(|_| ()),
        ),
        (
            "suite.json",
            r#"{"cases":"private-suite-canary","metadata":{"token":"adjacent-private-canary"}}"#,
            r#"{"id":"valid-suite","cases":[]}"#,
            "eval suite manifest",
            |path| load_eval_suite_manifest(path).map(|_| ()),
        ),
        (
            "pack.json",
            r#"{"version":"private-json-canary","metadata":{"token":"adjacent-private-canary"}}"#,
            r#"{"version":1,"id":"valid-pack","cases":[]}"#,
            "eval pack JSON",
            |path| load_eval_pack_manifest(path).map(|_| ()),
        ),
        (
            "pack.toml",
            "version = \"private-toml-canary\"\n[metadata]\ntoken = \"adjacent-private-canary\"",
            "version = 1\nid = \"valid-pack\"\ncases = []",
            "eval pack TOML",
            |path| load_eval_pack_manifest(path).map(|_| ()),
        ),
    ];
    let temp = tempfile::tempdir().unwrap();
    for (name, malformed, valid, label, load) in cases {
        let path = temp.path().join(name);
        std::fs::write(&path, malformed).unwrap();
        assert_private_parse_error(load(&path).expect_err("file must reach its parser"), label);
        std::fs::write(&path, valid).unwrap();
        load(&path).expect("valid file must still load");
    }
}

fn replay_manifest() -> EvalPackManifest {
    EvalPackManifest {
        id: "privacy-regression".into(),
        trials: 1,
        cases: vec![EvalPackCase {
            id: Some("replay".into()),
            run: Some("run".into()),
            fixture: Some("fixture".into()),
            ..Default::default()
        }],
        fixtures: vec![
            EvalPackFixtureRef {
                id: "run".into(),
                inline: Some(json!({"id": "valid-run", "status": "success"})),
                ..Default::default()
            },
            EvalPackFixtureRef {
                id: "fixture".into(),
                inline: Some(json!({"expected_status": "success"})),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

#[test]
fn public_eval_pack_imports_reject_private_inline_and_file_fixture_values() {
    let temp = tempfile::tempdir().unwrap();
    for (index, malformed, label) in [
        (
            0,
            json!({"tool_recordings": [{"duration_ms": "private-run-canary"}]}),
            "inline run record",
        ),
        (
            1,
            json!({"clarifying_question": {"min_questions": "private-fixture-canary"}}),
            "inline replay fixture",
        ),
    ] {
        let mut manifest = replay_manifest();
        manifest.fixtures[index].inline = Some(malformed);
        assert_private_parse_error(
            evaluate_eval_pack_manifest(&manifest).expect_err("inline fixture must be decoded"),
            label,
        );
    }

    let path = temp.path().join("fixture.json");
    let mut manifest = replay_manifest();
    manifest.fixtures[1].inline = None;
    manifest.fixtures[1].path = Some(path.to_string_lossy().into_owned());
    std::fs::write(
        &path,
        r#"{"clarifying_question":{"min_questions":"private-file-fixture-canary"}}"#,
    )
    .unwrap();
    assert_private_parse_error(
        evaluate_eval_pack_manifest(&manifest).expect_err("file fixture must be decoded"),
        "replay fixture",
    );
    std::fs::write(&path, r#"{"expected_status":"success"}"#).unwrap();
    for manifest in [replay_manifest(), manifest] {
        let report = evaluate_eval_pack_manifest(&manifest).expect("valid fixtures must evaluate");
        assert_eq!(report.total, 1);
        assert_eq!(report.trial_count, 1);
        assert_eq!(report.cases[0].run_id, "valid-run");
        assert!(report.pass, "{report:?}");
    }
}

#[test]
fn public_eval_ledger_arguments_do_not_echo_private_invalid_values() {
    assert_private_parse_error(
        eval_ledger_read_report(Some(json!({"limit": "private-limit-canary"})))
            .expect_err("options must be decoded before accessing the ledger"),
        "eval ledger options",
    );
    for (rows, label) in [
        (json!({"trial": "private-row-canary"}), "eval ledger row"),
        (json!([{"trial": "private-row-canary"}]), "eval ledger rows"),
    ] {
        assert_private_parse_error(
            eval_ledger_append_rows_report(rows, None)
                .expect_err("rows must be decoded before accessing the ledger"),
            label,
        );
    }
}

#[test]
fn public_transcript_descriptor_errors_keep_location_without_source_payload() {
    let temp = tempfile::tempdir().unwrap();
    let run_path = temp.path().join("run.json");
    let transcript_path = temp.path().join("transcript.jsonl");
    let run = RunRecord {
        id: "valid-run".into(),
        ..Default::default()
    };
    std::fs::write(&transcript_path, "\n{\"private-source-canary\":}\n").unwrap();
    let error = describe_llm_transcript_sidecar(&run, &run_path, &transcript_path)
        .expect_err("malformed row must be parsed");
    assert_eq!(error.kind, "malformed_jsonl");
    assert_eq!(
        error.message,
        format!(
            "{}:2 Runtime error: transcript JSONL row parse error: payload does not match expected schema",
            transcript_path.display()
        )
    );
    std::fs::write(
        &transcript_path,
        "{\"type\":\"agent_session_finalized\",\"terminal\":true}\n",
    )
    .unwrap();
    let descriptor = describe_llm_transcript_sidecar(&run, &run_path, &transcript_path)
        .expect("valid transcript must still be described");
    assert_eq!(descriptor.event_count, 1);
    assert!(descriptor.complete);
}

#[tokio::test(flavor = "current_thread")]
async fn public_run_eval_builtins_do_not_echo_private_fixture_values() {
    let mut vm = crate::Vm::new();
    crate::register_vm_stdlib(&mut vm);
    let run = json!({"id": "valid-run", "status": "success"});
    for (fixture, valid) in [
        (
            json!({"clarifying_question": {"min_questions": "private-fixture-canary"}}),
            false,
        ),
        (json!({"expected_status": "success"}), true),
    ] {
        for (builtin, args) in [
            ("run_record_eval", vec![run.clone(), fixture.clone()]),
            (
                "run_record_eval_suite",
                vec![json!([{"run": run, "fixture": fixture}])],
            ),
        ] {
            let result = vm
                .call_named_builtin(builtin, args.iter().map(crate::json_to_vm_value).collect())
                .await;
            if valid {
                let report =
                    crate::llm::vm_value_to_json(&result.expect("valid fixture must evaluate"));
                assert_eq!(report["pass"], true, "{builtin}: {report}");
                if builtin == "run_record_eval_suite" {
                    assert_eq!(report["total"], 1);
                    assert_eq!(report["cases"][0]["run_id"], "valid-run");
                }
            } else {
                assert_private_parse_error(
                    result.expect_err("private invalid fixture must reach public parser"),
                    &format!("{builtin} fixture"),
                );
            }
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_agent_replay_import_rejects_private_invalid_events() {
    use crate::event_log::{AnyEventLog, EventLog, LogEvent, MemoryEventLog, Topic};

    let topic = Topic::new("observability.agent_events.privacy-regression").unwrap();
    for (event, valid) in [
        (json!({"type": "private-event-canary"}), false),
        (
            json!({"type": "agent_message_chunk", "session_id": "privacy-regression", "content": "valid"}),
            true,
        ),
    ] {
        let log = AnyEventLog::Memory(MemoryEventLog::new(4));
        let event_id = log
            .append(
                &topic,
                LogEvent {
                    kind: "agent_event".into(),
                    payload: json!({"event": event}),
                    headers: std::collections::BTreeMap::from([(
                        "session_id".into(),
                        "privacy-regression".into(),
                    )]),
                    occurred_at_ms: 0,
                },
            )
            .await
            .unwrap();
        let result = load_agent_session_replay_events_from_log(&log, "privacy-regression").await;
        if valid {
            let events = result.expect("valid event must replay");
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].event_id, event_id);
        } else {
            assert_private_parse_error(
                result.expect_err("private unknown event type must reach parser"),
                &format!("agent event replay record {event_id}"),
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn public_eval_ledger_reader_rejects_private_invalid_event_rows() {
    use crate::event_log::{AnyEventLog, EventLog, LogEvent, MemoryEventLog, Topic};
    use std::sync::Arc;

    let topic = Topic::new("eval.ledger.privacy-regression").unwrap();
    let options = json!({"namespace": "privacy-regression"});
    for (row, valid) in [
        (json!({"trial": "private-ledger-trial-canary"}), false),
        (
            json!({"schema": "harn.eval.ledger.row.v2", "trial": 1}),
            true,
        ),
    ] {
        let log = Arc::new(AnyEventLog::Memory(MemoryEventLog::new(4)));
        let event_id = log
            .append(
                &topic,
                LogEvent {
                    kind: "eval.ledger.row".into(),
                    payload: row,
                    headers: Default::default(),
                    occurred_at_ms: 0,
                },
            )
            .await
            .unwrap();
        let previous = crate::event_log::swap_active_event_log(Some(log));
        let result = eval_ledger_read_report(Some(options.clone()));
        crate::event_log::swap_active_event_log(previous);
        if valid {
            let report = result.expect("valid event row must load");
            assert_eq!(report.rows.len(), 1);
            assert_eq!(report.rows[0].event_id, Some(event_id));
            assert_eq!(report.rows[0].trial, 1);
        } else {
            assert_private_parse_error(
                result.expect_err("private invalid event row must reach parser"),
                &format!("eval ledger row {event_id}"),
            );
        }
    }
}
