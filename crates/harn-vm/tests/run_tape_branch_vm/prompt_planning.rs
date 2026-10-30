use super::*;
use harn_vm::run_tape::{plan_prompt_branch, PromptEditActivation, PromptSourceEdit};
use harn_vm::testbench::tape::TapeRecordKind;

const FIRST: &str = "The first attributable instruction is long enough to identify.";
const SECOND: &str = "A separate instruction is consumed only by the third model call.";
const UNUSED: &str = "This rendered instruction is never sent to any model request.";

fn origin_edit(tape: &EventTape, original: &str, replacement: &str) -> PromptSourceEdit {
    let mut edit = prompt_edits::edit_for(tape, original, replacement);
    edit.activation = PromptEditActivation::Origin;
    edit
}

fn program(workspace: &Path, included: bool, ambiguous: bool, early: bool, answer: &str) -> String {
    let first = quote(&workspace.join(if included {
        "root.harn.prompt"
    } else {
        "first.harn.prompt"
    }));
    let second = quote(&workspace.join("second.harn.prompt"));
    let unused = quote(&workspace.join("unused.harn.prompt"));
    let short = quote(&workspace.join("short.harn.prompt"));
    let other = quote(&workspace.join("other.harn.prompt"));
    let extra = if ambiguous {
        format!("const other = harness.fs.render_prompt({other}, {{}})")
    } else {
        String::new()
    };
    let render = format!("const first = harness.fs.render_prompt({first}, {{}})");
    let initial = if early { render.as_str() } else { "" };
    let later = if early { "" } else { render.as_str() };
    format!(
        r#"
import {{ with_llm_script }} from "std/testing"
pipeline main(harness: Harness, task: unknown) {{
  with_llm_script(harness.llm, [{{text: "{answer}", model: "mock"}}, {{text: "{answer}", model: "mock"}}, {{text: "{answer}", model: "mock"}}], {{ ->
    const unused = harness.fs.render_prompt({unused}, {{}})
    {initial}
    const short = harness.fs.render_prompt({short}, {{}})
    const early = harness.llm.call(short, nil, {{provider: "mock", model: "mock"}})
    harness.stdio.println(early.text)
    {extra}
    {later}
    const middle = harness.llm.call(first, nil, {{provider: "mock", model: "mock"}})
    harness.stdio.println(middle.text)
    const second = harness.fs.render_prompt({second}, {{}})
    const last = harness.llm.call(second, nil, {{provider: "mock", model: "mock"}})
    harness.stdio.println(last.text)
  }})
}}
"#
    )
}

fn fixtures(workspace: &Path) {
    for (name, text) in [
        ("first", FIRST),
        ("other", FIRST),
        ("second", SECOND),
        ("unused", UNUSED),
        ("short", "short"),
        ("root", "{{ include \"first.harn.prompt\" }}"),
    ] {
        std::fs::write(workspace.join(format!("{name}.harn.prompt")), text).unwrap();
    }
}

#[test]
fn source_planning_chooses_first_consumers_and_runs_that_verified_prefix() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    for (included, early) in [(false, false), (true, false), (false, true), (true, true)] {
        let dirs = dirs();
        fixtures(&dirs.workspace);
        let baseline = program(&dirs.workspace, included, false, early, "baseline");
        assert_eq!(
            record(&baseline, &dirs.tapes, &dirs.workspace),
            "baseline\nbaseline\nbaseline\n"
        );
        let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
        let edits = vec![
            origin_edit(&tape, SECOND, "Edited third-call instruction."),
            origin_edit(&tape, FIRST, "Edited second-call instruction."),
        ];
        let point = plan_prompt_branch(&tape, &edits).unwrap();
        assert_eq!(point.first_live_call, 2);
        assert_eq!(point.live_after_calls, 1);
        assert_eq!(point.first_consumers, vec![3, 2]);
        let guard = start_branch(BranchPlan {
            tape,
            workspace_root: dirs.workspace.clone(),
            state_root: dirs.markers.join("planned-state"),
            live_after_calls: point.live_after_calls,
            host_state_roots: Vec::new(),
            prompt_edits: edits,
        })
        .unwrap();
        let output_dir = dirs.markers.join("planned-result");
        let recording = start_recording(
            &output_dir,
            RecordingLevel::Full,
            Some(TapeWorkspace {
                root: dirs.workspace.clone(),
                observer: Box::new(NoChanges),
                state_roots: Vec::new(),
            }),
        )
        .unwrap()
        .unwrap();
        let output = run_in(
            &program(&dirs.workspace, included, false, early, "fresh"),
            &dirs.workspace,
            Some(guard.scope()),
        )
        .unwrap();
        let report = guard.finish();
        assert_eq!(output, "baseline\nfresh\nfresh\n");
        assert_eq!(report.divergence, None);
        assert_eq!((report.served_calls, report.live_calls), (1, 2));
        assert!(report
            .prompt_edits
            .iter()
            .all(|edit| edit.prepared_renders == 1));
        assert_eq!(recording.error(), None);
        drop(recording);
        let result = EventTape::load(&output_dir.join(TAPE_FILE_NAME)).unwrap();
        let messages: Vec<_> = result
            .records
            .iter()
            .filter_map(|record| {
                if let TapeRecordKind::LlmCall { messages, .. } = &record.kind {
                    Some(messages)
                } else {
                    None
                }
            })
            .flatten()
            .map(|payload| {
                serde_json::from_slice::<serde_json::Value>(
                    &result.resolve_payload(payload).unwrap(),
                )
                .unwrap()
            })
            .collect();
        for expected in [
            "Edited second-call instruction.",
            "Edited third-call instruction.",
        ] {
            assert!(
                messages
                    .iter()
                    .any(|message| message["content"] == expected),
                "{messages:?}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(dirs.workspace.join("first.harn.prompt")).unwrap(),
            FIRST
        );
        assert_eq!(
            std::fs::read_to_string(dirs.workspace.join("second.harn.prompt")).unwrap(),
            SECOND
        );
    }
}

#[test]
fn source_planning_refuses_missing_and_ambiguous_request_evidence() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    for ambiguous in [false, true] {
        let dirs = dirs();
        fixtures(&dirs.workspace);
        record(
            &program(&dirs.workspace, false, ambiguous, false, "baseline"),
            &dirs.tapes,
            &dirs.workspace,
        );
        let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
        let edits = vec![origin_edit(&tape, FIRST, "Edited instruction.")];
        if ambiguous {
            let refusal = plan_prompt_branch(&tape, &edits).unwrap_err();
            assert!(
                refusal.contains("ambiguous request attribution at model call 2"),
                "{refusal}"
            );
        } else {
            assert_eq!(
                plan_prompt_branch(&tape, &edits).unwrap().first_live_call,
                2
            );
            for original in [UNUSED, "short"] {
                let edit = origin_edit(&tape, original, "Edited instruction.");
                let refusal = plan_prompt_branch(&tape, &[edit]).unwrap_err();
                assert!(
                    refusal.contains("1 prompt edits have no attributed model request"),
                    "{refusal}"
                );
            }
            let mut missing = tape.clone();
            let mut removed = 0;
            for record in &mut missing.records {
                if let TapeRecordKind::LlmCall { attribution, .. } = &mut record.kind {
                    removed += usize::from(attribution.take().is_some());
                }
            }
            assert_eq!(removed, 3);
            assert!(plan_prompt_branch(&missing, &edits)
                .unwrap_err()
                .contains("1 prompt edits have no attributed model request"));
            assert!(plan_prompt_branch(&tape, &[])
                .unwrap_err()
                .contains("at least one prompt edit"));
        }
    }
}
