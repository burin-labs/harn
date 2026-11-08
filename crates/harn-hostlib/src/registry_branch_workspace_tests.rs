//! Same-tape concurrent replay through the hostlib command owner.

use super::*;
use crate::workspace_checkpoint::{ReplayWorkspace, SeedLimits};
use harn_vm::run_tape::{start_branch, BranchPlan, BranchScope};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, at: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn execute(source: String, root: PathBuf, branch: Option<BranchScope>) -> Result<String, String> {
    harn_vm::on_vm_stack(move || {
        harn_vm::reset_thread_local_state();
        let chunk = harn_vm::compile_source(&source)?;
        let rt = harn_clock::test_support::current_thread_runtime();
        rt.block_on(harn_clock::test_support::with_local_tasks(async move {
            let run = async {
                harn_vm::stdlib::process::set_thread_execution_context(Some(
                    harn_vm::orchestration::RunExecutionRecord {
                        cwd: Some(root.display().to_string()),
                        project_root: Some(root.display().to_string()),
                        ..Default::default()
                    },
                ));
                let mut vm = harn_vm::new_vm();
                harn_runtime::register_vm_stdlib(&mut vm);
                registry().register_into_vm(&mut vm);
                vm.set_harness(harn_vm::Harness::real());
                let policy = harn_vm::orchestration::CapabilityPolicy {
                    workspace_roots: vec![root.display().to_string()],
                    sandbox_profile: harn_vm::orchestration::SandboxProfile::WorkspacePaths,
                    ..Default::default()
                };
                harn_vm::orchestration::scope_execution_policy(policy, vm.execute(&chunk))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(vm.output().to_string())
            };
            match branch {
                Some(branch) => branch.run(run).await,
                None => run.await,
            }
        }))
    })
}

fn source(root: &Path, value: &str) -> String {
    let root = serde_json::to_string(&root.display().to_string()).unwrap();
    let value = serde_json::to_string(value).unwrap();
    let command = serde_json::to_string("printf '%s' \"$REPLAY_VALUE\" > value").unwrap();
    format!(
        r#"
import {{ with_llm_script }} from "std/testing"
fn main(harness: Harness) {{
  with_llm_script(harness.llm, [{{text: {root}, model: "mock"}}, {{text: "live", model: "mock"}}], {{ ->
    const cwd = harness.tools.run_command({{argv: ["/bin/pwd"], cwd: {root}, env_mode: "replace"}})
    const prefix = harness.llm.call("workspace ${{cwd.stdout}}", {root}, {{provider: "mock", model: "mock"}})
    harness.stdio.println(prefix.text)
    harness.llm.call("continue", nil, {{provider: "mock", model: "mock"}})
    const written = harness.tools.run_command({{argv: ["/bin/sh", "-c", {command}], cwd: {root}, env_mode: "replace", env: {{REPLAY_VALUE: {value}}}}})
    assert(written.exit_code == 0)
    const read = harness.tools.read_file({{path: path_join({root}, "value")}})
    assert(read.content == {value})
    const search = harness.tools.search({{pattern: {value}, path: {root}, fixed_strings: true}})
    assert(len(search.matches) > 0)
  }})
}}
"#
    )
}

#[test]
fn concurrent_branches_of_one_tape_keep_grades_and_tape_bytes_independent() {
    const NAME: &str = "registry::run_tape_tests::branch_workspace::concurrent_branches_of_one_tape_keep_grades_and_tape_bytes_independent";
    const WORKER: &str = "HARN_TEST_CONCURRENT_BRANCH_WORKER";
    if std::env::var(WORKER).as_deref() != Ok(NAME) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
            .env(WORKER, NAME)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("both replay grades verified"));
        return;
    }
    let original = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let tapes = tempfile::tempdir().unwrap();
    std::fs::write(original.path().join("value"), "seed").unwrap();
    let mut captured =
        ReplayWorkspace::capture_in(original.path(), storage.path(), SeedLimits::default())
            .unwrap();
    let saved = captured.persist();
    let recorded_root = captured.path().to_path_buf();
    let recording = start_recording(
        tapes.path(),
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: recorded_root.clone(),
            observer: Box::new(NoChanges),
            state_roots: Vec::new(),
        }),
    )
    .unwrap()
    .unwrap();
    execute(
        source(&recorded_root, "baseline"),
        recorded_root.clone(),
        None,
    )
    .unwrap();
    assert_eq!(recording.error(), None);
    drop(recording);
    drop(captured);
    let before = snapshot(tapes.path());
    assert!(!before.is_empty());
    let saved_before = snapshot(saved.storage_path());
    let start = Arc::new(std::sync::Barrier::new(2));
    let grade = Arc::new(std::sync::Barrier::new(2));
    let arms: Vec<_> = ["left", "right"]
        .into_iter()
        .map(|value| {
            let saved = saved.clone();
            let parent = storage.path().to_path_buf();
            let tapes = tapes.path().to_path_buf();
            let recorded_root = recorded_root.clone();
            let start = start.clone();
            let grade = grade.clone();
            harn_parser::runtime_stack::spawn(move || {
                start.wait();
                let prepared = (|| {
                    let workspace = ReplayWorkspace::fork_saved_in(&saved, &parent)
                        .map_err(|e| e.to_string())?;
                    let guard = start_branch(BranchPlan {
                        tape: EventTape::load(&tapes.join(TAPE_FILE_NAME))?,
                        live_after_calls: 1,
                        workspace_root: workspace.path().to_path_buf(),
                        workspace_container_root: workspace.storage_parent().to_path_buf(),
                        state_root: parent.join(format!("state-{value}")),
                        host_state_roots: Vec::new(),
                        prompt_edits: Vec::new(),
                    })?;
                    let output = execute(
                        source(&recorded_root, value),
                        workspace.path().to_path_buf(),
                        Some(guard.scope()),
                    );
                    Ok::<_, String>((workspace, guard, output))
                })();
                // Both live commands finish before either arm reads its grade.
                grade.wait();
                let (workspace, guard, output) = prepared.unwrap();
                let output = output.unwrap();
                assert!(output.contains(recorded_root.to_str().unwrap()), "{output}");
                assert!(
                    !output.contains(workspace.path().to_str().unwrap()),
                    "{output}"
                );
                assert_eq!(
                    std::fs::read_to_string(workspace.path().join("value")).unwrap(),
                    value
                );
                let report = guard.finish();
                assert_eq!(
                    (
                        report.served_calls,
                        report.live_calls,
                        report.served_tool_calls
                    ),
                    (1, 1, 1)
                );
                assert_eq!(report.divergence, None);
                workspace.path().to_path_buf()
            })
        })
        .collect();
    let roots: Vec<_> = arms.into_iter().map(|arm| arm.join().unwrap()).collect();
    assert_ne!(roots[0], roots[1]);
    // A recorded branch is itself a valid baseline. Keep literal source paths
    // unchanged while descendants retain the original logical root.
    let mut parent_tape = tapes.path().to_path_buf();
    let mut descendants = Vec::new();
    for generation in 1..=2 {
        let workspace = ReplayWorkspace::fork_saved_in(&saved, storage.path()).unwrap();
        let next = tempfile::tempdir().unwrap();
        let guard = start_branch(BranchPlan {
            tape: EventTape::load(&parent_tape.join(TAPE_FILE_NAME)).unwrap(),
            live_after_calls: 1,
            workspace_root: workspace.path().to_path_buf(),
            workspace_container_root: workspace.storage_parent().to_path_buf(),
            state_root: storage.path().join(format!("descendant-{generation}")),
            host_state_roots: Vec::new(),
            prompt_edits: Vec::new(),
        })
        .unwrap();
        let scope = guard.scope();
        let _entered = scope.enter();
        let recording = start_recording(
            next.path(),
            RecordingLevel::Full,
            Some(TapeWorkspace {
                root: workspace.path().to_path_buf(),
                observer: Box::new(NoChanges),
                state_roots: Vec::new(),
            }),
        )
        .unwrap()
        .unwrap();
        let output = execute(
            source(&recorded_root, "descendant"),
            workspace.path().to_path_buf(),
            Some(scope.clone()),
        )
        .expect("a recorded branch must remain branchable");
        assert!(output.contains(recorded_root.to_str().unwrap()), "{output}");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("value")).unwrap(),
            "descendant"
        );
        assert_eq!(guard.finish().divergence, None);
        assert_eq!(recording.error(), None);
        drop(recording);
        parent_tape = next.path().to_path_buf();
        descendants.push(next);
    }
    assert_eq!(snapshot(tapes.path()), before);
    assert_eq!(snapshot(saved.storage_path()), saved_before);
    println!("both replay grades verified");
}
