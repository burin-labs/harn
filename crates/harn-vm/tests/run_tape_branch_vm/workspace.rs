use super::*;

#[test]
fn relocated_live_commands_cannot_touch_recorded_absolute_paths() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let marker = recorded.workspace.join("command-marker");
    let source = format!(
        r#"
import {{ with_llm_script }} from "std/testing"
pipeline main(harness: Harness, task: unknown) {{
  with_llm_script(harness.llm, [{{text: "prefix", model: "mock"}}, {{text: "live", model: "mock"}}], {{ ->
    harness.llm.call("prefix", nil, {{provider: "mock", model: "mock"}})
    harness.llm.call("live", nil, {{provider: "mock", model: "mock"}})
    harness.process.run({{program: "touch", args: [{marker}]}})
  }})
}}
"#,
        marker = quote(&marker)
    );
    record(&source, &recorded.tapes, &recorded.workspace);
    assert!(marker.exists(), "the baseline must execute the command");
    std::fs::remove_file(&marker).unwrap();
    let replay = branch(&source, &recorded.tapes, &isolated.workspace, 1);
    assert_eq!(replay.report.served_calls, 1);
    assert_eq!(replay.report.live_calls, 1);
    assert!(replay
        .output
        .unwrap_err()
        .contains("require a filesystem namespace"));
    assert!(!marker.exists());
    assert!(!isolated.workspace.join("command-marker").exists());
}

#[test]
fn a_branch_in_another_workspace_preserves_recorded_workspace_identity() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let source = r#"
import { with_llm_script } from "std/testing"
pipeline main(harness: Harness, task: unknown) {
  harness.stdio.println(harness.fs.cwd())
  with_llm_script(harness.llm, [{text: "prefix", model: "mock"}, {text: "live", model: "mock"}], { ->
    harness.llm.call(harness.fs.cwd(), nil, {provider: "mock", model: "mock"})
    const answer = harness.llm.call("continue", nil, {provider: "mock", model: "mock"})
    harness.stdio.println(answer.text)
  })
}
"#;
    let baseline = record(source, &recorded.tapes, &recorded.workspace);
    assert!(baseline.contains("live"));
    assert!(baseline.contains(recorded.workspace.parent().unwrap().to_str().unwrap()));
    let moved = run(source, &isolated.workspace).unwrap();
    assert!(moved.contains(isolated.workspace.parent().unwrap().to_str().unwrap()));
    assert_ne!(baseline, moved, "the real execution contexts must differ");
    let replay = branch(source, &recorded.tapes, &isolated.workspace, 1);
    assert_eq!(replay.report.divergence, None, "{:?}", replay.output);
    assert!(replay.output.unwrap().contains("live"));
    assert_eq!(replay.report.served_calls, 1);
    assert_eq!(replay.report.live_calls, 1);
}

#[test]
fn recorded_absolute_paths_read_and_write_only_the_isolated_workspace() {
    isolated_filesystem_case(false, false);
}

#[cfg(unix)]
#[test]
fn a_recorded_path_cannot_follow_a_branch_symlink_into_the_original_workspace() {
    isolated_filesystem_case(true, false);
}

#[test]
fn a_recorded_path_cannot_escape_the_branch_with_a_parent_component() {
    isolated_filesystem_case(false, true);
}

fn isolated_filesystem_case(escaping_link: bool, parent_escape: bool) {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let record_root = recorded.workspace.canonicalize().unwrap();
    let branch_root = isolated.workspace.canonicalize().unwrap();
    std::fs::write(record_root.join("input.txt"), "seed").unwrap();
    std::fs::write(branch_root.join("input.txt"), "seed").unwrap();
    std::fs::write(record_root.join("message.harn.prompt"), "recorded prompt").unwrap();
    std::fs::write(branch_root.join("message.harn.prompt"), "recorded prompt").unwrap();
    let source = r#"
import { with_llm_script } from "std/testing"
pipeline main(harness: Harness, task: unknown) {
  const root = harness.fs.cwd()
  const input = harness.fs.read_text(path_join(root, "input.txt"))
  const prompt = harness.fs.render_prompt(path_join(root, "message.harn.prompt"), {})
  with_llm_script(harness.llm, [{text: "prefix", model: "mock"}, {text: "live", model: "mock"}], { ->
    harness.llm.call("${root}: ${input}: ${prompt}", nil, {provider: "mock", model: "mock"})
    harness.llm.call("continue", nil, {provider: "mock", model: "mock"})
    harness.fs.write_text(path_join(root, "result.txt"), "branch only")
  })
}
"#;
    name_session_store(&record_root);
    let recording = start_recording(
        &recorded.tapes,
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: record_root.clone(),
            observer: Box::new(NoChanges),
            state_roots: Vec::new(),
        }),
    )
    .unwrap()
    .unwrap();
    // The host spelling can differ from the canonical tape root, notably
    // /var versus /private/var on macOS.
    run_at(source, &record_root, None, &recorded.workspace).unwrap();
    assert_eq!(recording.error(), None);
    drop(recording);
    assert_eq!(
        std::fs::read_to_string(record_root.join("result.txt")).unwrap(),
        "branch only"
    );
    std::fs::remove_file(record_root.join("result.txt")).unwrap();
    std::fs::write(
        record_root.join("input.txt"),
        "original changed after recording",
    )
    .unwrap();
    std::fs::write(
        record_root.join("message.harn.prompt"),
        "original prompt changed",
    )
    .unwrap();
    #[cfg(unix)]
    if escaping_link {
        std::os::unix::fs::symlink(
            record_root.join("input.txt"),
            branch_root.join("result.txt"),
        )
        .unwrap();
    }
    let guard = start_branch(BranchPlan {
        tape: EventTape::load(&recorded.tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 1,
        workspace_root: branch_root.clone(),
        state_root: isolated.markers.join("state"),
        host_state_roots: Vec::new(),
    })
    .unwrap();
    let replay_source = if parent_escape {
        source.replace("result.txt", "../escaped-result.txt")
    } else {
        source.to_string()
    };
    let output = run_at(
        &replay_source,
        &branch_root,
        Some(guard.scope()),
        &branch_root,
    );
    let report = guard.finish();
    assert_eq!(report.divergence, None, "{output:?}");
    if escaping_link || parent_escape {
        assert!(output
            .unwrap_err()
            .contains("escapes the isolated workspace"));
    } else {
        output.unwrap();
        assert_eq!(
            std::fs::read_to_string(branch_root.join("result.txt")).unwrap(),
            "branch only"
        );
    }
    assert_eq!(report.served_calls, 1);
    assert_eq!(report.live_calls, 1);
    assert!(!record_root.join("result.txt").exists());
    assert!(!record_root.join("../escaped-result.txt").exists());
    assert!(!branch_root.join("../escaped-result.txt").exists());
    assert_eq!(
        std::fs::read_to_string(record_root.join("input.txt")).unwrap(),
        "original changed after recording"
    );
}
