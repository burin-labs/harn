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
            let mut mid_run = edits[0].clone();
            mid_run.activation = PromptEditActivation::AfterPrefix;
            assert!(plan_prompt_branch(&tape, &[mid_run])
                .unwrap_err()
                .contains("requires origin activation"));
        }
    }
}

#[test]
fn an_origin_edit_cannot_bypass_verification_with_a_later_boundary() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    fixtures(&dirs.workspace);
    let source = program(&dirs.workspace, true, false, true, "baseline");
    record(&source, &dirs.tapes, &dirs.workspace);
    let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
    let edit = origin_edit(&tape, FIRST, "Edited instruction must reach verification.");
    assert_eq!(
        plan_prompt_branch(&tape, std::slice::from_ref(&edit))
            .unwrap()
            .first_live_call,
        2
    );
    let guard = start_branch(BranchPlan {
        tape,
        workspace_root: dirs.workspace.clone(),
        state_root: dirs.markers.join("incorrect-later-boundary"),
        live_after_calls: 2,
        host_state_roots: Vec::new(),
        prompt_edits: vec![edit],
    })
    .unwrap();
    assert!(run_in(&source, &dirs.workspace, Some(guard.scope())).is_err());
    let report = guard.finish();
    assert_eq!((report.served_calls, report.live_calls), (1, 0));
    let divergence = report
        .divergence
        .expect("changed request must refuse the recorded response");
    assert_eq!(divergence.call, 2);
    assert_eq!(divergence.what, "request");
    assert_eq!(
        std::fs::read_to_string(dirs.workspace.join("first.harn.prompt")).unwrap(),
        FIRST
    );
}

#[test]
fn source_planning_refuses_redacted_offsets_before_accepting_literal_proofs() {
    use harn_vm::stdlib::template::{PromptSourceSpan, PromptSpanKind};

    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    for redacted in [false, true] {
        let dirs = dirs();
        let target = dirs.workspace.join("redaction.harn.prompt");
        let template = format!("{{{{ prefix }}}}{FIRST}{{{{ padding }}}}");
        std::fs::write(&target, &template).unwrap();
        let prefix = if redacted {
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD"
        } else {
            "Public prefix."
        };
        let program = format!(
            r#"
import {{ with_llm_script }} from "std/testing"
pipeline main(harness: Harness, task: unknown) {{
  with_llm_script(harness.llm, [{{text: "baseline", model: "mock"}}], {{ ->
    const prompt = harness.fs.render_prompt({}, {{prefix: "{prefix}", padding: "{}"}})
    const response = harness.llm.call(prompt, nil, {{provider: "mock", model: "mock"}})
    harness.stdio.println(response.text)
  }})
}}
"#,
            quote(&target),
            "padding ".repeat(40),
        );
        assert_eq!(record(&program, &dirs.tapes, &dirs.workspace), "baseline\n");
        let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
        let (rendered, spans) = tape
            .records
            .iter()
            .find_map(|record| match &record.kind {
                TapeRecordKind::PromptRender {
                    rendered, spans, ..
                } => Some((rendered, spans)),
                _ => None,
            })
            .expect("the real render was recorded");
        let rendered = tape.resolve_payload(rendered).unwrap();
        let spans: Vec<PromptSourceSpan> =
            serde_json::from_slice(&tape.resolve_payload(spans).unwrap()).unwrap();
        let literal = spans
            .iter()
            .find(|span| span.kind == PromptSpanKind::Text)
            .unwrap();
        let stored_slice =
            std::str::from_utf8(&rendered[literal.output_start..literal.output_end]).unwrap();
        let edit = PromptSourceEdit {
            template_uri: literal.template_uri.clone(),
            template_revision: literal.template_revision.clone(),
            line: literal.template_line,
            column: literal.template_col,
            original: FIRST.to_string(),
            replacement: "Edited instruction.".to_string(),
            activation: PromptEditActivation::Origin,
        };
        let redactions = tape
            .records
            .iter()
            .filter(|record| matches!(record.kind, TapeRecordKind::PayloadRedaction))
            .count();
        if redacted {
            assert!(redactions > 0, "the recording must exercise real redaction");
            assert_ne!(
                stored_slice, FIRST,
                "redaction must shift the literal offsets"
            );
            assert!(!String::from_utf8_lossy(&rendered).contains(prefix));
            let refusal = plan_prompt_branch(&tape, std::slice::from_ref(&edit)).unwrap_err();
            assert!(refusal.contains("redacted payloads"), "{refusal}");
            // Previously these shifted bytes could be certified as the source
            // literal even though they include text from the padding expression.
            let forged = PromptSourceEdit {
                original: stored_slice.to_string(),
                ..edit.clone()
            };
            assert_eq!(plan_prompt_branch(&tape, &[forged]).unwrap_err(), refusal);
            let admission = start_branch(BranchPlan {
                tape,
                workspace_root: dirs.workspace.clone(),
                state_root: dirs.markers.join("redacted-state"),
                live_after_calls: 0,
                host_state_roots: Vec::new(),
                prompt_edits: vec![edit],
            })
            .err()
            .expect("redacted replay must also refuse");
            assert_eq!(admission, refusal);
        } else {
            assert_eq!(redactions, 0);
            assert_eq!(stored_slice, FIRST);
            assert_eq!(
                plan_prompt_branch(&tape, &[edit]).unwrap().first_live_call,
                1
            );
        }
        assert_eq!(std::fs::read_to_string(&target).unwrap(), template);
    }
}
