use super::*;

fn file_only_script(hook: &Path, marker: &Path) -> String {
    let source = mark_script(hook, marker);
    let invocation = format!(
        "harness.process.run({{program: \"touch\", args: [{}]}})",
        quote(marker)
    );
    assert_eq!(source.matches(&invocation).count(), 1);
    source.replace(
        &invocation,
        &format!("harness.fs.write_text({}, \"file only\")", quote(marker)),
    )
}

fn plan(tape: EventTape, workspace: &Path, k: u64) -> BranchPlan {
    BranchPlan {
        tape,
        live_after_calls: k,
        workspace: location(workspace),
        state_root: workspace.parent().unwrap().join("admission-state"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    }
}

#[test]
fn an_unconfined_relocated_command_carries_a_machine_readable_refusal() {
    use harn_vm::stdlib::host::dispatch_host_operation;
    use harn_vm::value::json::{json_to_vm_value, vm_value_to_json};

    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let source = r#"import { with_llm_script } from "std/testing"
        fn main(harness: Harness) {
          with_llm_script(harness.llm, [{text: "baseline"}], { ->
            harness.llm.call("baseline", nil, {provider: "mock", model: "mock"})
          })
        }"#;
    record(source, &recorded.tapes, &recorded.workspace);
    let tape = EventTape::load(&recorded.tapes.join(TAPE_FILE_NAME)).unwrap();
    let guard = start_branch(plan(tape, &isolated.workspace, 0)).unwrap();
    let marker = isolated.workspace.join("must-not-run");
    let request = json_to_vm_value(&serde_json::json!({
        "mode": "argv",
        "argv": ["touch", marker],
        "cwd": isolated.workspace,
    }));
    let workspace = isolated.workspace;
    harn_vm::on_vm_stack(move || {
        harn_vm::reset_thread_local_state();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(harn_clock::test_support::with_local_tasks(
            guard.scope().run(Box::pin(async move {
                let chunk = harn_vm::compile_source(source).unwrap();
                let mut vm = harn_vm::new_vm();
                harn_runtime::register_vm_stdlib(&mut vm);
                harn_vm::register_store_builtins(&mut vm, &workspace);
                vm.execute(&chunk)
                    .await
                    .expect("reach the live suffix before the command");
                let error = dispatch_host_operation("process", "exec", request.as_dict().unwrap())
                    .await
                    .expect_err("a moved branch without a confinement policy cannot spawn");
                assert_eq!(
                    harn_vm::value::error_to_category(&error),
                    harn_vm::value::ErrorCategory::ToolRejected,
                    "{error:?}"
                );
                let data = vm_value_to_json(&error.thrown_value());
                assert_eq!(
                    data["reason_code"], "replay_relocation_unconfined",
                    "{data}"
                );
                assert!(
                    data["message"]
                        .as_str()
                        .is_some_and(|message| !message.is_empty()),
                    "{data}"
                );
                assert!(!marker.exists(), "the refused command must never execute");
            })),
        ));
        assert_eq!(
            guard.report().live_calls,
            1,
            "the command must reach the live suffix"
        );
    });
}

#[test]
fn command_effects_measure_execution_inside_identically_named_tools() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    for command in [true, false] {
        let dirs = dirs();
        let hook = dirs.markers.join("hook");
        let marker = dirs.markers.join("effect");
        let source = mark_script(&hook, &marker);
        let invocation = format!(
            "harness.process.run({{program: \"touch\", args: [{}]}})",
            quote(&marker)
        );
        assert_eq!(source.matches(&invocation).count(), 1);
        let source = if command {
            source
        } else {
            source.replace(
                &invocation,
                &format!("harness.fs.write_text({}, \"file only\")", quote(&marker)),
            )
        };
        assert!(!marker.exists());
        record(&source, &dirs.tapes, &dirs.workspace);
        assert!(marker.is_file(), "the actual handler never ran");
        assert_eq!(std::fs::read_to_string(hook).unwrap(), "hook ran");
        if !command {
            assert_eq!(std::fs::read_to_string(marker).unwrap(), "file only");
        }
        let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
        let effects: Vec<_> = tape
            .records
            .iter()
            .filter_map(|record| match &record.kind {
                TapeRecordKind::ToolCall { tool, .. } if tool == "mark" => {
                    Some(serde_json::to_value(&record.kind).unwrap()["command_effect"].clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            effects,
            [serde_json::json!(if command {
                "requested"
            } else {
                "none"
            })]
        );
    }
}

#[test]
fn native_background_spawn_records_its_command_and_unservable_handle() {
    use harn_vm::stdlib::host::dispatch_host_operation;
    use harn_vm::value::json::{json_to_vm_value, vm_value_to_json};

    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let dirs = dirs();
    let marker = dirs.markers.join("native-background");
    let recording = start_recording(
        &dirs.tapes,
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: dirs.workspace.clone(),
            observer: Box::new(NoChanges),
            state_roots: Vec::new(),
        }),
    )
    .unwrap()
    .unwrap();
    run(
        r#"
import { with_llm_script } from "std/testing"
fn main(harness: Harness) {
  with_llm_script(harness.llm, [{text: "baseline"}], { ->
    harness.llm.call("baseline", nil, {provider: "mock", model: "mock"})
  })
}
"#,
        &dirs.workspace,
    )
    .unwrap();
    assert!(!marker.exists());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(harn_clock::test_support::with_local_tasks(async {
        enter_run(&dirs.workspace, dirs._root.path(), &[], &[]);
        let request = json_to_vm_value(&serde_json::json!({
            "mode": "argv",
            "argv": ["sh", "-c", "printf core-background > \"$1\"", "fixture", marker],
            "cwd": dirs.workspace,
            "sandbox_profile": "workspace_paths"
        }));
        let handle = dispatch_host_operation("process", "spawn", request.as_dict().unwrap())
            .await
            .unwrap();
        let id = vm_value_to_json(&handle)["handle_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let request = json_to_vm_value(&serde_json::json!({"handle_id": id}));
        let waited = dispatch_host_operation("process", "wait", request.as_dict().unwrap())
            .await
            .unwrap();
        let waited = vm_value_to_json(&waited);
        assert_eq!(waited["status"], "exited");
        assert_eq!(waited["exit_code"], 0);
        dispatch_host_operation("process", "release", request.as_dict().unwrap())
            .await
            .unwrap();
        leave_run();
    }));
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "core-background");
    assert_eq!(recording.error(), None);
    drop(recording);
    let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
    let commands: Vec<_> = tape
        .records
        .iter()
        .filter_map(|record| match &record.kind {
            TapeRecordKind::ToolCall {
                command_effect,
                unservable,
                ..
            } => Some((*command_effect, unservable)),
            _ => None,
        })
        .collect();
    assert_eq!(
        commands.len(),
        1,
        "a real native background command must be measured"
    );
    assert_eq!(
        commands[0].0,
        Some(harn_vm::testbench::tape::CommandEffect::Requested)
    );
    assert!(commands[0]
        .1
        .as_deref()
        .is_some_and(|reason| reason.contains("background")));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let isolated = super::dirs();
        let error = match start_branch(plan(tape, &isolated.workspace, 0)) {
            Ok(_) => panic!("native background command evidence was lost at admission"),
            Err(error) => error,
        };
        assert!(
            error.contains("cannot replay commands in a moved workspace"),
            "{error}"
        );
    }
}

#[test]
fn missing_command_evidence_refuses_even_in_the_live_suffix() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let source = file_only_script(
        &recorded.markers.join("hook"),
        &recorded.markers.join("file"),
    );
    record(&source, &recorded.tapes, &recorded.workspace);
    let tape = EventTape::load(&recorded.tapes.join(TAPE_FILE_NAME)).unwrap();
    assert_eq!(tape.header.version, 9);
    for k in [0, 1] {
        let mut unmeasured = tape.clone();
        let mut changed = 0;
        for record in &mut unmeasured.records {
            if let TapeRecordKind::ToolCall { command_effect, .. } = &mut record.kind {
                assert!(command_effect.take().is_some());
                changed += 1;
            }
        }
        assert_eq!(changed, 1, "the negative control must remove real evidence");
        let plan = plan(unmeasured, &isolated.workspace, k);
        let state = plan.state_root.clone();
        let error = match start_branch(plan) {
            Ok(_) => panic!("unmeasured commands were admitted at {k}"),
            Err(error) => error,
        };
        assert!(error.contains("did not measure command effects"), "{error}");
        assert!(
            !state.exists(),
            "admission must precede branch state creation"
        );
    }
}

#[test]
fn moved_file_only_replay_runs_live_without_command_support() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let source = file_only_script(
        &recorded.workspace.join("hook"),
        &recorded.workspace.join("file"),
    );
    record(&source, &recorded.tapes, &recorded.workspace);
    assert_eq!(
        std::fs::read_to_string(recorded.workspace.join("file")).unwrap(),
        "file only"
    );
    assert!(!isolated.workspace.join("file").exists());
    let replay = branch(&source, &recorded.tapes, &isolated.workspace, 0);
    replay
        .output
        .expect("filesystem-only replay remains available");
    assert_eq!(replay.report.divergence, None);
    assert_eq!(
        (replay.report.served_calls, replay.report.live_calls),
        (0, 2)
    );
    assert_eq!(
        std::fs::read_to_string(isolated.workspace.join("file")).unwrap(),
        "file only"
    );
    assert_eq!(
        std::fs::read_to_string(isolated.workspace.join("hook")).unwrap(),
        "hook ran"
    );
}

#[test]
fn moved_served_commands_keep_their_evidence_in_descendant_tapes() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let marker = recorded.markers.join("command");
    let hook = recorded.markers.join("hook");
    let source = mark_script(&hook, &marker);
    record(&source, &recorded.tapes, &recorded.workspace);
    remove_marker(&marker, &recorded.tapes);
    remove_marker(&hook, &recorded.tapes);
    let recording = start_recording(
        &isolated.tapes,
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: isolated.workspace.clone(),
            observer: Box::new(NoChanges),
            state_roots: Vec::new(),
        }),
    )
    .unwrap()
    .unwrap();
    let replay = branch(&source, &recorded.tapes, &isolated.workspace, 1);
    replay
        .output
        .expect("a served command needs no live child process");
    assert_eq!(replay.report.divergence, None);
    assert_eq!(
        (replay.report.served_calls, replay.report.live_calls),
        (1, 1)
    );
    assert_eq!(replay.report.served_tool_calls, 1);
    assert!(!marker.exists(), "the command must not run again");
    assert!(!hook.exists(), "the hook must not run again");
    assert_eq!(recording.error(), None);
    drop(recording);
    let tape = EventTape::load(&isolated.tapes.join(TAPE_FILE_NAME)).unwrap();
    let effects: Vec<_> = tape
        .records
        .iter()
        .filter_map(|record| match &record.kind {
            TapeRecordKind::ToolCall { command_effect, .. } => Some(*command_effect),
            _ => None,
        })
        .collect();
    assert_eq!(
        effects,
        [Some(harn_vm::testbench::tape::CommandEffect::Requested)]
    );
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let descendant = dirs();
        let error = match start_branch(plan(tape, &descendant.workspace, 0)) {
            Ok(_) => panic!("descendant lost its recorded command evidence"),
            Err(error) => error,
        };
        assert!(
            error.contains("cannot replay commands in a moved workspace"),
            "{error}"
        );
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn moved_recorded_live_commands_refuse_before_arming_or_running_a_model() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let marker = recorded.markers.join("command");
    let source = mark_script(&recorded.markers.join("hook"), &marker);
    record(&source, &recorded.tapes, &recorded.workspace);
    assert!(
        marker.is_file(),
        "the baseline must execute the real command"
    );
    remove_marker(&marker, &recorded.tapes);
    let tape = EventTape::load(&recorded.tapes.join(TAPE_FILE_NAME)).unwrap();
    let original_source = tempfile::tempdir().unwrap();
    let restored_source = tempfile::tempdir().unwrap();
    for source_only in [false, true] {
        let workspace = if source_only {
            &recorded.workspace
        } else {
            &isolated.workspace
        };
        let roots = if source_only {
            vec![harn_vm::run_tape::ReplaySourceRoot {
                recorded: original_source.path().canonicalize().unwrap(),
                restored: restored_source.path().canonicalize().unwrap(),
            }]
        } else {
            Vec::new()
        };
        let plan = plan(tape.clone(), workspace, 0);
        let state = plan.state_root.clone();
        let error = match harn_vm::run_tape::start_branch_with_sources(plan, roots) {
            Ok(_) => {
                panic!("unsupported live command replay was admitted; source_only={source_only}")
            }
            Err(error) => error,
        };
        assert!(
            error.contains("cannot replay commands in a moved workspace"),
            "source_only={source_only}: {error}"
        );
        assert!(!state.exists());
        assert!(!marker.exists());
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[test]
fn a_new_live_command_still_reaches_the_runtime_refusal() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = dirs();
    let isolated = dirs();
    let marker = recorded.workspace.join("command");
    let file_write = format!("harness.fs.write_text({}, \"file only\")", quote(&marker));
    let source = format!(
        r#"
import {{ with_llm_script }} from "std/testing"
fn main(harness: Harness) {{
  with_llm_script(harness.llm, [{{text: "first"}}, {{text: "second"}}], {{ ->
    harness.llm.call("first", nil, {{provider: "mock", model: "mock"}})
    harness.llm.call("second", nil, {{provider: "mock", model: "mock"}})
    {file_write}
  }})
}}
"#
    );
    record(&source, &recorded.tapes, &recorded.workspace);
    assert_eq!(source.matches(&file_write).count(), 1);
    let live_source = source.replace(
        &file_write,
        &format!(
            "harness.process.run({{program: \"touch\", args: [{}]}})",
            quote(&marker)
        ),
    );
    let replay = branch(&live_source, &recorded.tapes, &isolated.workspace, 0);
    let result = match replay.output {
        Ok(output) => output,
        Err(error) => error,
    };
    assert!(
        result.contains("cannot replay commands in a moved workspace"),
        "{result}"
    );
    assert_eq!(
        replay.report.live_calls, 2,
        "admission must allow the measured file-only tape"
    );
    assert!(!isolated.workspace.join("command").exists());
}
