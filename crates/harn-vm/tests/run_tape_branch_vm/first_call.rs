use super::*;

fn source(prompt: &str, answer: &str) -> String {
    format!(
        r#"
import {{ with_llm_script }} from "std/testing"
pipeline main(harness: Harness, task: unknown) {{
  harness.stdio.println(harness.random.uuid())
  with_llm_script(harness.llm, [{{text: "{answer}", model: "mock"}}], {{ ->
    const answer = harness.llm.call("{prompt}", nil, {{provider: "mock", model: "mock"}})
    harness.stdio.println(answer.text)
  }})
}}
"#
    )
}

#[test]
fn first_call_edits_reach_live_inference_and_record_the_changed_request() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    let template = dirs.workspace.join("instruction.harn.prompt");
    std::fs::write(&template, "original").unwrap();
    let render = format!("harness.fs.render_prompt({}, {{}})", quote(&template));
    let baseline = source("original", "baseline").replace("\"original\"", &render);
    assert!(record(&baseline, &dirs.tapes, &dirs.workspace).contains("baseline"));
    std::fs::write(&template, "edited").unwrap();
    let output_tape = dirs.workspace.parent().unwrap().join("treatment-tape");
    let recording = start_recording(
        &output_tape,
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: dirs.workspace.clone(),
            observer: Box::new(NoChanges),
            state_roots: Vec::new(),
        }),
    )
    .unwrap()
    .unwrap();
    let replay = branch(
        &source("edited", "treatment").replace("\"edited\"", &render),
        &dirs.tapes,
        &dirs.workspace,
        0,
    );
    assert!(replay.output.unwrap().contains("treatment"));
    assert_eq!(replay.report.served_calls, 0);
    assert_eq!(replay.report.live_calls, 1);
    assert!(
        replay.report.served_reads > 0,
        "pre-inference reads were replayed"
    );
    assert_eq!(replay.report.divergence, None);
    assert_eq!(recording.error(), None);
    drop(recording);
    let tape = EventTape::load(&output_tape.join(TAPE_FILE_NAME)).unwrap();
    let calls: Vec<_> = tape
        .records
        .iter()
        .filter_map(|record| match &record.kind {
            harn_vm::testbench::tape::TapeRecordKind::LlmCall {
                messages, response, ..
            } => Some((
                messages
                    .iter()
                    .map(|message| {
                        serde_json::from_slice::<serde_json::Value>(
                            &tape.resolve_payload(message).unwrap(),
                        )
                        .unwrap()
                    })
                    .collect::<Vec<_>>(),
                serde_json::from_slice::<serde_json::Value>(
                    &tape.resolve_payload(response).unwrap(),
                )
                .unwrap(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1, "the live provider response was recorded");
    assert!(
        calls[0]
            .0
            .iter()
            .any(|message| message["content"] == "edited"),
        "{:?}",
        calls[0].0
    );
    assert_eq!(calls[0].1["text"], "treatment");
}

#[test]
fn first_call_branch_that_never_reaches_inference_is_incomplete() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    record(
        &source("original", "baseline"),
        &dirs.tapes,
        &dirs.workspace,
    );
    let replay = branch(
        "fn main(harness: Harness) { harness.stdio.println(harness.random.uuid()) }",
        &dirs.tapes,
        &dirs.workspace,
        0,
    );
    replay.output.unwrap();
    assert_eq!(replay.report.live_calls, 0);
    assert_eq!(replay.report.divergence.unwrap().what, "under-run branch");
}

#[test]
fn first_call_branch_refuses_an_unconsumed_pre_inference_read() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    let baseline = source("original", "baseline");
    record(&baseline, &dirs.tapes, &dirs.workspace);
    let treatment =
        source("edited", "treatment").replace("harness.stdio.println(harness.random.uuid())", "");
    let replay = branch(&treatment, &dirs.tapes, &dirs.workspace, 0);
    assert!(replay.output.is_err());
    assert_eq!(replay.report.live_calls, 0);
    assert_eq!(replay.report.divergence.unwrap().what, "under-run prefix");
}

#[test]
fn first_call_branch_requires_an_observed_baseline_call() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    record("fn main(harness: Harness) {}", &dirs.tapes, &dirs.workspace);
    let result = start_branch(BranchPlan {
        tape: EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 0,
        workspace_root: dirs.workspace,
        state_root: dirs.markers.join("branch-state"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    });
    assert_eq!(
        result.err().unwrap(),
        "a branch needs a recorded model call as its baseline"
    );
}
