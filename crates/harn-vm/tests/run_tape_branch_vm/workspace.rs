use super::*;

#[cfg(unix)]
#[test]
fn result_readers_report_branch_path_refusals_as_structured_errors() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    for read in [
        "harness.fs.read_text_result(path)",
        "harness.fs.read_lines_page_result(path, {})",
        "harness.fs.read_lines_append_page_result(path, {})",
    ] {
        let recorded = dirs();
        let isolated = dirs();
        let source = format!(
            r#"
import {{ with_llm_script }} from "std/testing"
pipeline main(harness: Harness, task: unknown) {{
  with_llm_script(harness.llm, [{{text: "prefix", model: "mock"}}, {{text: "live", model: "mock"}}], {{ ->
    harness.llm.call("prefix", nil, {{provider: "mock", model: "mock"}})
    harness.llm.call("live", nil, {{provider: "mock", model: "mock"}})
    const path = {path}
    const read = {read}
    if is_err(read) {{
      const failure = unwrap_err(read)
      harness.stdio.println("handled:${{failure.error}}:${{failure.kind}}")
    }} else {{
      harness.stdio.println("read:${{unwrap(read)}}")
    }}
  }})
}}
"#,
            path = quote(&recorded.workspace.join("input.txt"))
        );
        std::fs::write(recorded.workspace.join("input.txt"), "recorded-canary\n").unwrap();
        std::fs::write(isolated.workspace.join("input.txt"), "isolated-canary\n").unwrap();
        let baseline = record(&source, &recorded.tapes, &recorded.workspace);
        assert!(baseline.contains("recorded-canary"), "{baseline}");
        let positive = branch(&source, &recorded.tapes, &isolated.workspace, 1);
        let output = positive.output.unwrap();
        assert!(output.contains("isolated-canary"), "{output}");
        assert_eq!(
            (positive.report.served_calls, positive.report.live_calls),
            (1, 1)
        );
        std::fs::remove_file(isolated.workspace.join("input.txt")).unwrap();
        std::os::unix::fs::symlink(
            recorded.workspace.join("input.txt"),
            isolated.workspace.join("input.txt"),
        )
        .unwrap();
        let refused = branch(&source, &recorded.tapes, &isolated.workspace, 1);
        let output = refused
            .output
            .expect("result-returning readers must not throw");
        assert!(
            output.contains("handled:io_error:invalid_input"),
            "{output}"
        );
        assert!(
            !output.contains("canary"),
            "refused read leaked file content: {output}"
        );
        assert_eq!(
            (refused.report.served_calls, refused.report.live_calls),
            (1, 1)
        );
        assert_eq!(
            std::fs::read_to_string(recorded.workspace.join("input.txt")).unwrap(),
            "recorded-canary\n"
        );
    }
}

#[cfg(target_os = "linux")]
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
    const result = harness.process.run({{program: "touch", args: [{marker}]}})
    harness.stdio.println(result)
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
    let refusal = match replay.output {
        Ok(output) => output,
        Err(error) => error,
    };
    assert!(
        refusal.contains("denied") || refusal.contains("permitted"),
        "{refusal}"
    );
    assert!(!marker.exists());
    assert!(!isolated.workspace.join("command-marker").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn a_restored_script_cannot_address_its_embedded_recorded_path() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let marker = recorded.workspace.join("embedded-marker");
    let sibling = location(&isolated.workspace)
        .storage_pool()
        .join("sibling/workspace");
    std::fs::create_dir_all(&sibling).unwrap();
    let sibling_marker = sibling.join("embedded-marker");
    let script = format!(
        "printf recorded > '{}'\nprintf sibling > '{}'\n",
        marker.display(),
        sibling_marker.display()
    );
    std::fs::write(recorded.workspace.join("embedded.sh"), &script).unwrap();
    std::fs::write(isolated.workspace.join("embedded.sh"), &script).unwrap();
    let source = r#"
import { with_llm_script } from "std/testing"
pipeline main(harness: Harness, task: unknown) {
  with_llm_script(harness.llm, [{text: "prefix", model: "mock"}, {text: "live", model: "mock"}], { ->
    harness.llm.call("prefix", nil, {provider: "mock", model: "mock"})
    harness.llm.call("live", nil, {provider: "mock", model: "mock"})
    const result = harness.process.run({program: "sh", args: ["embedded.sh"]})
    harness.stdio.println(result)
  })
}
"#;
    record(source, &recorded.tapes, &recorded.workspace);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "recorded");
    assert_eq!(std::fs::read_to_string(&sibling_marker).unwrap(), "sibling");
    std::fs::remove_file(&marker).unwrap();
    std::fs::remove_file(&sibling_marker).unwrap();
    let replay = branch(source, &recorded.tapes, &isolated.workspace, 1);
    assert_eq!(
        (replay.report.served_calls, replay.report.live_calls),
        (1, 1)
    );
    let refusal = match replay.output {
        Ok(output) => output,
        Err(error) => error,
    };
    assert!(
        refusal.contains("denied") || refusal.contains("permitted"),
        "{refusal}"
    );
    assert!(
        !marker.exists(),
        "an opaque child reached the original tree"
    );
    assert!(
        !sibling_marker.exists(),
        "an opaque child reached a sibling arm"
    );
    assert!(
        !isolated.workspace.join("embedded-marker").exists(),
        "script bytes must not be edited to relocate a hidden path"
    );
    assert_eq!(
        std::fs::read_to_string(isolated.workspace.join("embedded.sh")).unwrap(),
        script
    );
}

#[cfg(target_os = "linux")]
#[test]
fn command_cwd_relocates_without_rewriting_arguments_or_environment() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let source = format!(
        r#"
import {{ with_llm_script }} from "std/testing"
pipeline main(harness: Harness, task: unknown) {{
  with_llm_script(harness.llm, [{{text: "prefix", model: "mock"}}, {{text: "live", model: "mock"}}], {{ ->
    harness.llm.call("prefix", nil, {{provider: "mock", model: "mock"}})
    harness.llm.call("live", nil, {{provider: "mock", model: "mock"}})
    const result = harness.process.run({{program: "sh", args: ["-c", {command}], cwd: {root}, env: {{BRANCH_INPUT: {root}}}}})
    harness.stdio.println(result)
  }})
}}
"#,
        root = quote(&recorded.workspace),
        command = serde_json::to_string(
            "printf '%s' \"$BRANCH_INPUT\" > projected-env; pwd > projected-cwd"
        )
        .unwrap()
    );
    record(&source, &recorded.tapes, &recorded.workspace);
    assert!(recorded.workspace.join("projected-env").exists());
    let replay = branch(&source, &recorded.tapes, &isolated.workspace, 1);
    replay.output.unwrap();
    let root = isolated
        .workspace
        .canonicalize()
        .unwrap()
        .display()
        .to_string();
    assert_eq!(
        std::fs::read_to_string(isolated.workspace.join("projected-env")).unwrap(),
        recorded.workspace.display().to_string()
    );
    assert_eq!(
        std::fs::read_to_string(isolated.workspace.join("projected-cwd"))
            .unwrap()
            .trim(),
        root
    );
    assert_eq!(
        (replay.report.served_calls, replay.report.live_calls),
        (1, 1)
    );
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
fn a_recorded_root_alias_keeps_its_identity_after_the_alias_is_removed() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let names = tempfile::tempdir().unwrap();
    let alias = names.path().join("recorded-alias");
    std::os::unix::fs::symlink(&recorded.workspace, &alias).unwrap();
    let source = format!(
        r#"
import {{ with_llm_script }} from "std/testing"
fn main(harness: Harness) {{
  with_llm_script(harness.llm, [{{text: "prefix", model: "mock"}}, {{text: "live", model: "mock"}}], {{ ->
    harness.llm.call({root}, nil, {{provider: "mock", model: "mock"}})
    harness.llm.call("live", nil, {{provider: "mock", model: "mock"}})
    harness.fs.write_text(path_join({root}, "alias-result"), "branch only")
  }})
}}
"#,
        root = quote(&alias)
    );
    let recording = start_recording(
        &recorded.tapes,
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: alias.clone(),
            observer: Box::new(NoChanges),
            state_roots: Vec::new(),
        }),
    )
    .unwrap()
    .unwrap();
    run_at(&source, &alias, None, &alias).unwrap();
    assert_eq!(recording.error(), None);
    drop(recording);
    std::fs::remove_file(alias).unwrap();
    let replay = branch(&source, &recorded.tapes, &isolated.workspace, 1);
    assert_eq!(replay.report.divergence, None, "{:?}", replay.output);
    replay.output.unwrap();
    assert_eq!(
        (replay.report.served_calls, replay.report.live_calls),
        (1, 1)
    );
    assert_eq!(
        std::fs::read_to_string(isolated.workspace.join("alias-result")).unwrap(),
        "branch only"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_removed_nested_alias_keeps_its_child_read_denial_in_the_branch() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let names = tempfile::tempdir().unwrap();
    let parent_alias = names.path().join("parent-alias");
    std::os::unix::fs::symlink(&recorded.markers, &parent_alias).unwrap();
    let direct_alias = recorded.markers.join("recorded-alias");
    std::os::unix::fs::symlink(&recorded.workspace, &direct_alias).unwrap();
    let alias = parent_alias.join("recorded-alias");
    for workspace in [&recorded.workspace, &isolated.workspace] {
        std::fs::create_dir(workspace.join("readonly")).unwrap();
    }
    std::fs::write(recorded.workspace.join("readonly/data"), "recorded canary").unwrap();
    let source = format!(
        r#"
import {{ with_llm_script }} from "std/testing"
fn main(harness: Harness) {{
  with_llm_script(harness.llm, [{{text: "prefix", model: "mock"}}, {{text: "live", model: "mock"}}], {{ ->
    harness.llm.call({root}, nil, {{provider: "mock", model: "mock"}})
    harness.llm.call("live", nil, {{provider: "mock", model: "mock"}})
    const result = harness.process.run({{program: "cat", args: ["readonly/data"]}})
    harness.stdio.println(result)
  }})
}}
"#,
        root = quote(&alias),
    );
    let recording = start_recording(
        &recorded.tapes,
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: alias.clone(),
            observer: Box::new(NoChanges),
            state_roots: Vec::new(),
        }),
    )
    .unwrap()
    .unwrap();
    assert!(run_at(&source, &alias, None, &alias)
        .unwrap()
        .contains("recorded canary"));
    assert_eq!(recording.error(), None);
    drop(recording);
    std::fs::remove_file(direct_alias).unwrap();
    for (case, denied) in [("read-control", false), ("read-denied", true)] {
        let target = isolated.workspace.join("readonly/data");
        std::fs::write(&target, "private canary").unwrap();
        let guard = start_branch(BranchPlan {
            tape: EventTape::load(&recorded.tapes.join(TAPE_FILE_NAME)).unwrap(),
            live_after_calls: 1,
            workspace: location(&isolated.workspace),
            state_root: isolated.markers.join(case),
            host_state_roots: Vec::new(),
            prompt_edits: Vec::new(),
        })
        .unwrap();
        let output = run_at_with_path_grants(
            &source,
            &isolated.workspace,
            Some(guard.scope()),
            &isolated.workspace,
            Vec::new(),
            if denied {
                vec![alias.join("readonly")]
            } else {
                Vec::new()
            },
        );
        let report = guard.finish();
        assert_eq!((report.served_calls, report.live_calls), (1, 1));
        assert_eq!(report.divergence, None);
        let output = output.unwrap_or_else(|error| error);
        if denied {
            assert!(
                !output.contains("private canary"),
                "read denial was lost: {output}"
            );
            assert!(
                output.contains("denied") || output.contains("permitted"),
                "{output}"
            );
        } else {
            assert!(output.contains("private canary"), "{output}");
        }
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "private canary");
    }
    assert_eq!(
        std::fs::read_to_string(recorded.workspace.join("readonly/data")).unwrap(),
        "recorded canary"
    );
}

#[test]
fn live_file_reads_cannot_address_a_peer_in_the_storage_pool() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let peer = location(&isolated.workspace)
        .storage_pool()
        .join("peer-canary");
    std::fs::write(&peer, "peer content").unwrap();
    let source = format!(
        r#"
import {{ with_llm_script }} from "std/testing"
fn main(harness: Harness) {{
  with_llm_script(harness.llm, [{{text: "prefix", model: "mock"}}, {{text: "live", model: "mock"}}], {{ ->
    harness.llm.call("prefix", nil, {{provider: "mock", model: "mock"}})
    harness.llm.call("live", nil, {{provider: "mock", model: "mock"}})
    harness.stdio.println(harness.fs.read_text({peer}))
  }})
}}
"#,
        peer = quote(&peer)
    );
    let reads = vec![location(&isolated.workspace).storage_pool().to_path_buf()];
    name_session_store(&recorded.workspace);
    let recording = start_recording(
        &recorded.tapes,
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: recorded.workspace.clone(),
            observer: Box::new(NoChanges),
            state_roots: Vec::new(),
        }),
    )
    .unwrap()
    .unwrap();
    let baseline = run_at_with_read_roots(
        &source,
        &recorded.workspace,
        None,
        &recorded.workspace,
        reads.clone(),
    )
    .unwrap();
    assert!(baseline.contains("peer content"));
    assert_eq!(recording.error(), None);
    drop(recording);
    let guard = start_branch(BranchPlan {
        tape: EventTape::load(&recorded.tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 1,
        workspace: location(&isolated.workspace),
        state_root: isolated.markers.join("peer-state"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    })
    .unwrap();
    let refusal = run_at_with_read_roots(
        &source,
        &isolated.workspace,
        Some(guard.scope()),
        &isolated.workspace,
        reads,
    )
    .unwrap_err();
    let report = guard.finish();
    assert!(refusal.contains("another replay workspace"), "{refusal}");
    assert_eq!((report.served_calls, report.live_calls), (1, 1));
    assert_eq!(std::fs::read_to_string(peer).unwrap(), "peer content");
}

#[cfg(target_os = "linux")]
#[test]
fn live_commands_keep_explicit_read_only_assets_outside_the_storage_pool() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let assets = tempfile::tempdir().unwrap();
    let helper = assets.path().join("helper-input");
    std::fs::write(&helper, "retained asset content").unwrap();
    let source = format!(
        r#"
import {{ with_llm_script }} from "std/testing"
pipeline main(harness: Harness, task: unknown) {{
  with_llm_script(harness.llm, [{{text: "prefix", model: "mock"}}, {{text: "live", model: "mock"}}], {{ ->
    harness.llm.call("prefix", nil, {{provider: "mock", model: "mock"}})
    harness.llm.call("live", nil, {{provider: "mock", model: "mock"}})
    const read = harness.process.run({{program: "cat", args: [{helper}]}})
    assert(read.stdout == "retained asset content")
    harness.stdio.println(read.stdout)
  }})
}}
"#,
        helper = quote(&helper)
    );
    assert!(
        record(&source, &recorded.tapes, &recorded.workspace).contains("retained asset content")
    );
    let guard = start_branch(BranchPlan {
        tape: EventTape::load(&recorded.tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 1,
        workspace: location(&isolated.workspace),
        state_root: isolated.markers.join("asset-state"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    })
    .unwrap();
    let output = run_at_with_read_roots(
        &source,
        &isolated.workspace,
        Some(guard.scope()),
        &isolated.workspace,
        vec![assets.path().to_path_buf()],
    )
    .unwrap();
    assert!(output.contains("retained asset content"));
    let report = guard.finish();
    assert_eq!((report.served_calls, report.live_calls), (1, 1));
    assert_eq!(report.divergence, None);
}

#[test]
fn retained_source_metadata_cannot_reopen_a_peer_workspace() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let original = recorded.markers.join("source-package");
    let pool = location(&isolated.workspace).storage_pool().to_path_buf();
    let peer = pool.join("peer-package");
    for root in [&original, &peer] {
        std::fs::create_dir(root).unwrap();
    }
    std::fs::write(original.join("asset"), "original source").unwrap();
    std::fs::write(peer.join("asset"), "peer source canary").unwrap();
    let source = format!(
        r#"
import {{ with_llm_script }} from "std/testing"
fn main(harness: Harness) {{
  with_llm_script(harness.llm, [{{text: "prefix", model: "mock"}}, {{text: "live", model: "mock"}}], {{ ->
    harness.llm.call("prefix", nil, {{provider: "mock", model: "mock"}})
    harness.llm.call("live", nil, {{provider: "mock", model: "mock"}})
    harness.stdio.println(harness.fs.read_text({asset}))
  }})
}}
"#,
        asset = quote(&original.join("asset")),
    );
    assert!(record(&source, &recorded.tapes, &recorded.workspace).contains("original source"));
    let mut tape = EventTape::load(&recorded.tapes.join(TAPE_FILE_NAME)).unwrap();
    tape.header.run.as_mut().unwrap().source_workspaces =
        vec![harn_vm::run_tape::RecordedSourceRoot {
            root: peer.canonicalize().unwrap(),
            ancestors: vec![original.canonicalize().unwrap()],
        }];
    let state = isolated.markers.join("forged-source-state");
    match start_branch(BranchPlan {
        tape,
        live_after_calls: 1,
        workspace: location(&isolated.workspace),
        state_root: state.clone(),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    }) {
        Err(error) => {
            assert!(error.contains("storage pool"), "{error}");
            assert!(!state.exists());
        }
        Ok(guard) => {
            let output = run_at_with_read_roots(
                &source,
                &isolated.workspace,
                Some(guard.scope()),
                &isolated.workspace,
                vec![pool],
            )
            .unwrap();
            assert!(output.contains("peer source canary"), "{output}");
            panic!("retained source metadata exposed the peer: {output}");
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn source_only_relocation_confines_commands_to_the_current_package() {
    use harn_vm::run_tape::{start_branch_with_sources, ReplaySourceRoot};
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let assets = tempfile::tempdir().unwrap();
    let original = assets.path().join("original");
    let current = assets.path().join("current");
    for root in [&original, &current] {
        std::fs::create_dir(root).unwrap();
    }
    std::fs::write(original.join("asset"), "old package canary").unwrap();
    std::fs::write(current.join("asset"), "current package canary").unwrap();
    let alias = assets.path().join("original-alias");
    std::os::unix::fs::symlink(&original, &alias).unwrap();
    let source = |root: &Path| {
        format!(
            r#"
import {{ with_llm_script }} from "std/testing"
fn main(harness: Harness) {{
  with_llm_script(harness.llm, [{{text: "prefix", model: "mock"}}, {{text: "live", model: "mock"}}], {{ ->
    harness.llm.call("prefix", nil, {{provider: "mock", model: "mock"}})
    harness.llm.call("live", nil, {{provider: "mock", model: "mock"}})
    const result = harness.process.run({{program: "cat", args: [{asset}]}})
    harness.stdio.println(result)
  }})
}}
"#,
            asset = quote(&root.join("asset")),
        )
    };
    let recording = start_recording(
        &recorded.tapes,
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: recorded.workspace.clone(),
            observer: Box::new(NoChanges),
            state_roots: Vec::new(),
        }),
    )
    .unwrap()
    .unwrap();
    let baseline = run_at_with_read_roots(
        &source(&original),
        &recorded.workspace,
        None,
        &recorded.workspace,
        vec![assets.path().to_path_buf()],
    )
    .unwrap();
    assert!(baseline.contains("old package canary"), "{baseline}");
    assert_eq!(recording.error(), None);
    drop(recording);
    for (arm, workspace) in [&recorded.workspace, &isolated.workspace]
        .into_iter()
        .enumerate()
    {
        for (case, argv, grant, success) in [
            ("broad-old", &original, assets.path(), false),
            ("exact-old", &original, current.as_path(), false),
            ("exact-current", &current, current.as_path(), true),
            ("alias-old-argv", &original, alias.as_path(), false),
            ("alias-current-argv", &current, alias.as_path(), true),
        ] {
            let guard = start_branch_with_sources(
                BranchPlan {
                    tape: EventTape::load(&recorded.tapes.join(TAPE_FILE_NAME)).unwrap(),
                    live_after_calls: 1,
                    workspace: location(workspace),
                    state_root: isolated
                        .markers
                        .join(format!("source-command-{arm}-{case}")),
                    host_state_roots: Vec::new(),
                    prompt_edits: Vec::new(),
                },
                vec![ReplaySourceRoot {
                    recorded: original.clone(),
                    restored: current.clone(),
                }],
            )
            .unwrap();
            let output = run_at_with_read_roots(
                &source(argv),
                workspace,
                Some(guard.scope()),
                workspace,
                vec![grant.to_path_buf()],
            );
            let report = guard.finish();
            assert_eq!((report.served_calls, report.live_calls), (1, 1));
            assert_eq!(report.divergence, None);
            let output = output.unwrap_or_else(|error| error);
            assert!(
                !output.contains("old package canary"),
                "arm {arm} {case}: {output}"
            );
            if success {
                assert!(output.contains("current package canary"), "{output}");
            } else {
                assert!(
                    output.contains("cannot admit grant")
                        || output.contains("denied")
                        || output.contains("permitted"),
                    "arm {arm} {case}: {output}"
                );
            }
        }
    }
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
        workspace: location(&branch_root),
        state_root: isolated.markers.join("state"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
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
