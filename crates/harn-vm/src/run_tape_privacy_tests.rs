use super::*;

const SECRET: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD";

fn private_args(purpose: &str) -> serde_json::Value {
    serde_json::json!({"authorization": SECRET, "text": format!("Bearer {SECRET}"), "purpose": purpose})
}

fn private_recording(dir: &Path) -> EventTape {
    let recording = record_full(dir);
    let mut response = result("dispatch", None);
    response.tool_calls = vec![serde_json::json!({
        "id": "private-tool", "name": "read", "arguments": private_args("read")
    })];
    record_llm_call(&request(&format!("inspect {SECRET}")), &response);
    cache_lookup(SECRET, SECRET, || {
        Ok(Some(serde_json::json!({"value": SECRET})))
    })
    .unwrap();
    let tool = begin_tool_call("private-tool", "read", &private_args("read"), true).unwrap();
    finish_tool_call(
        tool,
        Ok(ok(
            serde_json::json!({"answer": format!("Bearer {SECRET}")}),
        )),
    );
    record_llm_call(
        &request(&format!("answer Bearer {SECRET}")),
        &result("done", None),
    );
    assert_eq!(recording.error(), None);
    drop(recording);
    let tape = EventTape::load(&dir.join(TAPE_FILE_NAME)).unwrap();
    assert!(tape
        .records
        .iter()
        .any(|record| matches!(record.kind, TapeRecordKind::PayloadRedaction { .. })));
    tape
}

fn private_branch(dir: &Path, tape: EventTape) -> Result<TestBranch, String> {
    branch_from_tape(tape, 2, dir)
}

#[test]
fn redacted_prefix_serves_model_tool_and_cache_without_reaching_live_handlers() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let fixture = replay_fixture();
    let tape = private_recording(fixture.path());
    let branch = private_branch(fixture.path(), tape).expect("normalized redacted prefix");
    let response = serve_llm_call(&request(&format!("inspect {SECRET}")))
        .unwrap()
        .unwrap();
    let args = &response.tool_calls[0]["arguments"];
    assert!(!args.to_string().contains(SECRET));
    let cached = cache_lookup(SECRET, SECRET, || panic!("recorded cache read went live"))
        .unwrap()
        .unwrap();
    assert!(!cached.to_string().contains(SECRET));
    let ToolServe::Served(RecordedToolResult::Ok { value }, _, _, _) =
        serve_tool_call("private-tool", "read", args)
    else {
        panic!("redacted tool arguments did not match the recorded dispatch");
    };
    let answer = value["answer"].as_str().unwrap();
    assert!(!answer.contains(SECRET));
    assert!(answer.contains("<redacted:"));
    let terminal = serve_llm_call(&request(&format!("answer {answer}")))
        .unwrap()
        .unwrap();
    assert_eq!(terminal.text, "done");
    let report = branch.finish();
    assert_eq!(
        (
            report.served_calls,
            report.served_tool_calls,
            report.live_calls,
            report.live_cache_reads
        ),
        (2, 1, 0, 0)
    );
    assert_eq!(report.divergence, None);
}

#[test]
fn redaction_does_not_hide_changed_nonsecret_model_fields_or_leak_diagnostics() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let fixture = replay_fixture();
    let tape = private_recording(fixture.path());
    let _branch = private_branch(fixture.path(), tape).unwrap();
    let error = serve_llm_call(&request(&format!("changed {SECRET}")))
        .unwrap()
        .unwrap_err();
    assert!(!error.to_string().contains(SECRET));
    assert_eq!(branch_report().unwrap().served_calls, 0);
    assert!(branch_report().unwrap().divergence.is_some());
    assert!(serve_llm_call(&request("attempt to go live"))
        .unwrap()
        .is_err());
}

#[test]
fn redaction_does_not_hide_changed_nonsecret_tool_arguments() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let fixture = replay_fixture();
    let tape = private_recording(fixture.path());
    let _branch = private_branch(fixture.path(), tape).unwrap();
    serve_llm_call(&request(&format!("inspect {SECRET}")))
        .unwrap()
        .unwrap();
    assert!(matches!(
        serve_tool_call("private-tool", "read", &private_args("changed")),
        ToolServe::Diverged(_)
    ));
    let report = branch_report().unwrap();
    assert_eq!(report.served_tool_calls, 0);
    assert!(report.divergence.is_some());
}

#[test]
fn legacy_redacted_tapes_remain_readable_but_cannot_claim_normalized_digests() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let fixture = replay_fixture();
    let mut tape = private_recording(fixture.path());
    tape.header.version = 9;
    let path = fixture.path().join("legacy.tape");
    tape.persist(&path).unwrap();
    let loaded = EventTape::load(&path).unwrap();
    assert!(private_branch(fixture.path(), loaded)
        .err()
        .is_some_and(|error| error.contains("format 9")));
}

#[test]
fn discarded_typed_metadata_cannot_be_mistaken_for_a_complete_prefix() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let fixture = replay_fixture();
    let recording = record_full(fixture.path());
    record_llm_call(&request("unchanged"), &result("done", None));
    let mut tool = begin_tool_call("typed-tool", "read", &serde_json::json!({}), true).unwrap();
    tool.transcript_changes.push(TranscriptChange::Fields {
        session_id: "session".into(),
        fields: BTreeMap::from([(
            "token".into(),
            crate::testbench::tape::FieldChange::Set(serde_json::json!(SECRET)),
        )]),
    });
    finish_tool_call(tool, Ok(ok(serde_json::json!("done"))));
    drop(recording);
    let tape = EventTape::load(&fixture.path().join(TAPE_FILE_NAME)).unwrap();
    assert!(tape.records.iter().any(|record| matches!(
        record.kind,
        TapeRecordKind::PayloadRedaction {
            replay: crate::testbench::tape::RedactionReplay::InvalidMetadata
        }
    )));
    assert!(private_branch(fixture.path(), tape)
        .err()
        .is_some_and(|error| error.contains("InvalidMetadata")));
}

#[test]
fn a_redaction_without_measured_eligibility_never_means_safe_replay() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let fixture = replay_fixture();
    let mut tape = private_recording(fixture.path());
    let kind: TapeRecordKind =
        serde_json::from_value(serde_json::json!({"kind": "payload_redaction"})).unwrap();
    assert!(matches!(
        kind,
        TapeRecordKind::PayloadRedaction {
            replay: crate::testbench::tape::RedactionReplay::Unmeasured
        }
    ));
    tape.records[0].kind = kind;
    assert!(private_branch(fixture.path(), tape)
        .err()
        .is_some_and(|error| error.contains("Unmeasured")));
}

#[test]
fn missing_cache_lookup_redacts_both_error_and_divergence_report() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let fixture = replay_fixture();
    let tape = private_recording(fixture.path());
    let branch = private_branch(fixture.path(), tape).unwrap();
    let error = cache_lookup(SECRET, SECRET, || {
        panic!("known lookup at the wrong position went live")
    })
    .unwrap_err();
    assert!(!error.to_string().contains(SECRET));
    let divergence = branch
        .finish()
        .divergence
        .expect("the wrong position must diverge");
    assert!(!divergence.detail.contains(SECRET));
    assert_eq!(divergence.what, "cache_lookup");
}

#[test]
fn missing_tool_redacts_both_error_and_divergence_report() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let fixture = replay_fixture();
    let tape = private_recording(fixture.path());
    let branch = private_branch(fixture.path(), tape).unwrap();
    serve_llm_call(&request(&format!("inspect {SECRET}")))
        .unwrap()
        .unwrap();
    let ToolServe::Diverged(error) = serve_tool_call("private-tool", SECRET, &private_args("read"))
    else {
        panic!("the unrecorded tool must diverge");
    };
    assert!(!error.to_string().contains(SECRET));
    let divergence = branch.finish().divergence.unwrap();
    assert!(!divergence.detail.contains(SECRET));
    assert_eq!(divergence.what, "tool call");
}
