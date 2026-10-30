use super::*;
use harn_vm::run_tape::PromptSourceEdit;
use harn_vm::stdlib::template::{PromptSourceSpan, PromptSpanKind};
use harn_vm::testbench::tape::TapeRecordKind;

fn program(target: &str, answer: &str) -> String {
    let target = serde_json::to_string(target).unwrap();
    format!(
        r#"
import {{ with_llm_script }} from "std/testing"
pipeline main(harness: Harness, task: unknown) {{
  with_llm_script(harness.llm, [{{text: "{answer}", model: "mock"}}, {{text: "{answer}", model: "mock"}}], {{ ->
    for index in [0, 1] {{
      const prompt = harness.fs.render_prompt({target}, {{}})
      const response = harness.llm.call(prompt, nil, {{provider: "mock", model: "mock"}})
      harness.stdio.println(prompt)
      harness.stdio.println(response.text)
    }}
  }})
}}
"#
    )
}

fn edit_for(tape: &EventTape, original: &str, replacement: &str) -> PromptSourceEdit {
    for record in &tape.records {
        if let TapeRecordKind::PromptRender {
            spans, rendered, ..
        } = &record.kind
        {
            let rendered = tape.resolve_payload(rendered).unwrap();
            let spans: Vec<PromptSourceSpan> =
                serde_json::from_slice(&tape.resolve_payload(spans).unwrap()).unwrap();
            for span in spans {
                if span.kind == PromptSpanKind::Text
                    && rendered.get(span.output_start..span.output_end) == Some(original.as_bytes())
                {
                    return PromptSourceEdit {
                        template_uri: span.template_uri,
                        template_revision: span.template_revision,
                        line: span.template_line,
                        column: span.template_col,
                        original: original.to_string(),
                        replacement: replacement.to_string(),
                    };
                }
            }
        }
    }
    panic!("the canonical recording contains no literal {original:?}");
}

fn plan(
    tape: EventTape,
    workspace: &Path,
    state: &Path,
    edit: PromptSourceEdit,
    k: u64,
) -> BranchPlan {
    BranchPlan {
        tape,
        workspace_root: workspace.to_path_buf(),
        state_root: state.to_path_buf(),
        live_after_calls: k,
        host_state_roots: Vec::new(),
        prompt_edits: vec![edit],
    }
}

#[test]
fn prompt_edits_preserve_the_prefix_and_change_live_requests_without_writing_sources() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    for included in [false, true] {
        let dirs = dirs();
        let leaf = dirs.workspace.join("leaf.harn.prompt");
        let root = dirs.workspace.join("root.harn.prompt");
        let source = "é{{ if true }}Old instruction.{{ end }}";
        std::fs::write(&leaf, source).unwrap();
        std::fs::write(&root, "{{ include \"leaf.harn.prompt\" }}").unwrap();
        let target = if included { &root } else { &leaf };
        let baseline = program(target.to_str().unwrap(), "baseline");
        assert_eq!(
            record(&baseline, &dirs.tapes, &dirs.workspace),
            "éOld instruction.\nbaseline\néOld instruction.\nbaseline\n"
        );
        let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
        let attributions: Vec<_> = tape
            .records
            .iter()
            .filter_map(|record| {
                if let TapeRecordKind::LlmCall { attribution, .. } = &record.kind {
                    attribution.as_ref()
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(attributions.len(), 2);
        assert!(
            attributions
                .iter()
                .all(|attribution| attribution.segments.is_empty()),
            "recording a short render must not manufacture request attribution"
        );
        let edit = edit_for(&tape, "Old instruction.", "New instruction.");
        let guard = start_branch(plan(
            tape,
            &dirs.workspace,
            &dirs.markers.join("state"),
            edit,
            1,
        ))
        .unwrap();
        let out_dir = dirs.markers.join("edited-tape");
        let recording = start_recording(
            &out_dir,
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
            &program(target.to_str().unwrap(), "fresh"),
            &dirs.workspace,
            Some(guard.scope()),
        )
        .unwrap();
        let report = guard.finish();
        assert_eq!(report.divergence, None);
        assert_eq!((report.served_calls, report.live_calls), (1, 1));
        assert_eq!(report.prompt_edits[0].prepared_renders, 1);
        assert_eq!(
            output,
            "éOld instruction.\nbaseline\néNew instruction.\nfresh\n"
        );
        assert_eq!(std::fs::read_to_string(&leaf).unwrap(), source);
        assert_eq!(recording.error(), None);
        drop(recording);
        let edited = EventTape::load(&out_dir.join(TAPE_FILE_NAME)).unwrap();
        let requests: Vec<_> = edited
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
                    &edited.resolve_payload(payload).unwrap(),
                )
                .unwrap()
            })
            .collect();
        assert!(
            requests
                .iter()
                .any(|request| request["content"] == "éNew instruction."),
            "{requests:?}"
        );
    }
}

#[test]
fn prompt_edits_refuse_changed_source_and_forged_recorded_coordinates() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    let path = dirs.workspace.join("prompt.harn.prompt");
    std::fs::write(&path, "Original").unwrap();
    let source = program(path.to_str().unwrap(), "baseline");
    record(&source, &dirs.tapes, &dirs.workspace);
    let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
    let edit = edit_for(&tape, "Original", "Edited");
    let mut forged = edit.clone();
    forged.column += 1;
    let refusal = start_branch(plan(
        tape.clone(),
        &dirs.workspace,
        &dirs.markers.join("forged"),
        forged,
        0,
    ));
    assert!(refusal.err().unwrap().contains("not a recorded literal"));
    assert!(!dirs.markers.join("forged").exists());
    let guard = start_branch(plan(
        tape,
        &dirs.workspace,
        &dirs.markers.join("stale"),
        edit,
        0,
    ))
    .unwrap();
    std::fs::write(&path, "Changed after recording").unwrap();
    assert!(run_in(&source, &dirs.workspace, Some(guard.scope()))
        .unwrap_err()
        .contains("changed since recording"));
    let report = guard.finish();
    assert_eq!(report.live_calls, 0);
    assert_eq!(report.prompt_edits[0].prepared_renders, 0);
    assert_eq!(report.divergence.unwrap().what, "prompt source edit");
}

#[test]
fn prompt_edits_that_are_never_loaded_are_an_incomplete_branch() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    let path = dirs.workspace.join("prompt.harn.prompt");
    std::fs::write(&path, "Original").unwrap();
    let source = program(path.to_str().unwrap(), "baseline");
    record(&source, &dirs.tapes, &dirs.workspace);
    let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
    let edit = edit_for(&tape, "Original", "Edited");
    let guard = start_branch(plan(
        tape,
        &dirs.workspace,
        &dirs.markers.join("unused"),
        edit,
        0,
    ))
    .unwrap();
    let source = source.replace(
        &format!("harness.fs.render_prompt({}, {{}})", quote(&path)),
        "\"unrelated\"",
    );
    assert!(run_in(&source, &dirs.workspace, Some(guard.scope())).is_ok());
    let report = guard.finish();
    assert_eq!(report.live_calls, 2);
    assert_eq!(report.prompt_edits[0].prepared_renders, 0);
    let divergence = report.divergence.unwrap();
    assert_eq!(divergence.what, "unapplied prompt edits");
    assert!(divergence.detail.contains("1 prompt edits"));
}

#[test]
fn prompt_edits_apply_to_embedded_assets_from_the_first_live_call() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    let target = "std/workflow/prompts/verification_context_intro.harn.prompt";
    let original = include_str!(
        "../../../harn-stdlib/src/stdlib/workflow/prompts/verification_context_intro.harn.prompt"
    );
    let source = program(target, "baseline");
    assert!(record(&source, &dirs.tapes, &dirs.workspace).contains(original));
    let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
    let edit = edit_for(&tape, original, "Edited embedded guidance.");
    assert!(edit.template_uri.starts_with("std://"));
    let guard = start_branch(plan(
        tape,
        &dirs.workspace,
        &dirs.markers.join("embedded"),
        edit,
        0,
    ))
    .unwrap();
    let output = run_in(
        &program(target, "fresh"),
        &dirs.workspace,
        Some(guard.scope()),
    )
    .unwrap();
    let report = guard.finish();
    assert_eq!(report.divergence, None);
    assert_eq!((report.served_calls, report.live_calls), (0, 2));
    assert_eq!(report.prompt_edits[0].prepared_renders, 2);
    assert_eq!(
        output,
        "Edited embedded guidance.\nfresh\nEdited embedded guidance.\nfresh\n"
    );
}

#[test]
fn prompt_edits_follow_recorded_paths_into_a_relocated_workspace() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    for alias in [false, true] {
        let dirs = dirs();
        let original = dirs.workspace.join("prompt.harn.prompt");
        std::fs::write(&original, "Original").unwrap();
        let target = if alias {
            let alias_root = dirs.markers.join("alias");
            std::os::unix::fs::symlink(&dirs.workspace, &alias_root).unwrap();
            alias_root.join("prompt.harn.prompt")
        } else {
            original.clone()
        };
        let source = program(target.to_str().unwrap(), "baseline");
        record(&source, &dirs.tapes, &dirs.workspace);
        let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
        let edit = edit_for(&tape, "Original", "Edited");
        let moved = dirs.markers.join("isolated");
        std::fs::create_dir(&moved).unwrap();
        std::fs::write(moved.join("prompt.harn.prompt"), "Original").unwrap();
        std::fs::write(&original, "Original workspace changed").unwrap();
        let guard = start_branch(plan(
            tape,
            &moved,
            &dirs.markers.join("moved-state"),
            edit,
            1,
        ))
        .unwrap();
        let output = run_at(&source, &moved, Some(guard.scope()), &moved).unwrap();
        let report = guard.finish();
        assert_eq!(report.divergence, None, "alias={alias}");
        assert_eq!((report.served_calls, report.live_calls), (1, 1));
        assert_eq!(report.prompt_edits[0].prepared_renders, 1);
        assert!(output.contains("Edited"));
        assert_eq!(
            std::fs::read_to_string(&original).unwrap(),
            "Original workspace changed"
        );
        assert_eq!(
            std::fs::read_to_string(moved.join("prompt.harn.prompt")).unwrap(),
            "Original"
        );
    }
}
