//! Branches of one recording, each in a directory of its own, running at once.
//! The pipeline and the model see the recorded workspace; file access and
//! commands resolve into the branch's own directory.

use super::*;
use harn_vm::orchestration::SandboxProfile;

const SESSION: &str = "own-root";

/// Records the files named `made*` at the workspace root after each call, as
/// a workspace observer records what a command wrote.
struct MadeFiles;

impl ToolEffectObserver for MadeFiles {
    fn begin(&self, _root: &Path, _call_id: &str) {}
    fn end(&self, root: &Path, _call_id: &str) -> EffectScan {
        let mut changes: Vec<_> = std::fs::read_dir(root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("made"))
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    Some(std::fs::read(entry.path()).unwrap()),
                )
            })
            .collect();
        changes.sort();
        EffectScan {
            changes,
            incomplete: false,
        }
    }
}

/// Run `source` as a host would: anchored at `workspace`, the directory the
/// host gives the session, with commands under `profile`.
fn run_anchored(
    source: &str,
    workspace: &Path,
    branch: Option<BranchScope>,
    profile: SandboxProfile,
) -> Result<String, String> {
    let source = source.to_string();
    let workspace = workspace.to_path_buf();
    harn_vm::on_vm_stack(move || {
        harn_vm::reset_thread_local_state();
        let chunk = harn_vm::compile_source(&source)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let body = async move {
            harn_vm::stdlib::process::set_thread_execution_context(Some(
                harn_vm::orchestration::RunExecutionRecord {
                    cwd: Some(workspace.display().to_string()),
                    project_root: Some(workspace.display().to_string()),
                    ..Default::default()
                },
            ));
            harn_vm::orchestration::push_execution_policy(
                harn_vm::orchestration::CapabilityPolicy {
                    workspace_roots: vec![workspace.display().to_string()],
                    sandbox_profile: profile,
                    ..Default::default()
                },
            );
            harn_vm::agent_sessions::open_or_create(Some(SESSION.to_string()))
                .map_err(|error| format!("{error:?}"))?;
            harn_vm::agent_sessions::set_workspace_anchor(
                SESSION,
                Some(harn_vm::workspace_anchor::WorkspaceAnchor {
                    primary: workspace.clone(),
                    additional_roots: Vec::new(),
                    anchored_at: "2026-01-01T00:00:00Z".to_string(),
                }),
            )?;
            let mut vm = harn_vm::new_vm();
            harn_runtime::register_vm_stdlib(&mut vm);
            harn_vm::register_store_builtins(&mut vm, &workspace);
            let result = vm
                .execute(&chunk)
                .await
                .map_err(|error: VmError| format!("{error:?}"));
            leave_run();
            result?;
            Ok(vm.output().to_string())
        };
        rt.block_on(harn_clock::test_support::with_local_tasks(async move {
            match branch {
                Some(branch) => branch.run(Box::pin(body)).await,
                None => Box::pin(body).await,
            }
        }))
    })
}

/// A seed workspace, recorded running the script `source` writes for it.
struct Recorded {
    dirs: Dirs,
    seed: PathBuf,
}

fn record_seed(source: impl Fn(&Path) -> String) -> Recorded {
    let dirs = dirs();
    let seed = dirs.workspace.canonicalize().unwrap();
    let source = source(&seed);
    std::fs::write(seed.join("input.txt"), "seed input\n").unwrap();
    name_session_store(&seed);
    let guard = start_recording(
        &dirs.tapes,
        RecordingLevel::Full,
        Some(TapeWorkspace {
            root: seed.clone(),
            observer: Box::new(MadeFiles),
            state_roots: Vec::new(),
        }),
    )
    .unwrap()
    .unwrap();
    let output = run_anchored(&source, &seed, None, SandboxProfile::WorkspacePaths)
        .expect("the recorded run succeeds");
    assert_eq!(guard.error(), None, "the tape wrote cleanly");
    drop(guard);
    assert!(output.contains("recorded"), "{output}");
    Recorded { dirs, seed }
}

/// A branch's own directory, holding a copy of the seed.
fn branch_root(seed: &Path, own: &Dirs) -> PathBuf {
    let root = own.workspace.canonicalize().unwrap();
    std::fs::copy(seed.join("input.txt"), root.join("input.txt")).unwrap();
    root
}

fn arm(recorded: &Recorded, root: &Path, state: &Path) -> harn_vm::run_tape::BranchGuard {
    start_branch(BranchPlan {
        tape: EventTape::load(&recorded.dirs.tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 1,
        workspace_root: root.to_path_buf(),
        state_root: state.to_path_buf(),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    })
    .unwrap()
}

/// Every file under `root`, with its contents, for comparing a tree before
/// and after.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    files.sort();
    files
}

/// The prefix: a session anchored where the host put it, a file read, a model
/// call naming the anchor, and a command run there that writes a file. The
/// live tail writes through the anchor.
const PREFIX_SCRIPT: &str = r#"
import { with_llm_script } from "std/testing"
pipeline main(harness: Harness, task: unknown) {
  const root = harness.agent.workspace_anchor("own-root").primary
  const input = harness.fs.read_text(path_join(root, "input.txt"))
  with_llm_script(harness.llm, [{text: "prefix", model: "mock"}, {text: "live", model: "mock"}], { ->
    harness.llm.call("anchored at ${root}: ${input}", nil, {provider: "mock", model: "mock"})
    harness.process.run({program: "sh", args: ["-c", "cat input.txt > made.txt"], cwd: root})
    const answer = harness.llm.call("continue", nil, {provider: "mock", model: "mock"})
    harness.fs.write_text(path_join(root, "result.txt"), answer.text)
    harness.stdio.println("recorded ${answer.text} at ${root}")
  })
}
"#;

#[test]
fn two_branches_of_one_recording_run_at_once_each_in_its_own_root() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let recorded = record_seed(|_| PREFIX_SCRIPT.to_string());
    let seed = &recorded.seed;
    assert_eq!(
        std::fs::read_to_string(seed.join("made.txt")).unwrap(),
        "seed input\n"
    );
    // The seed as a host keeps it: what it captured, without the recorded
    // run's writes.
    std::fs::remove_file(seed.join("made.txt")).unwrap();
    std::fs::remove_file(seed.join("result.txt")).unwrap();
    let seed_before = snapshot(seed);

    let (first, second) = (dirs(), dirs());
    let roots = [branch_root(seed, &first), branch_root(seed, &second)];
    let guards = [
        arm(&recorded, &roots[0], &first.markers.join("state")),
        arm(&recorded, &roots[1], &second.markers.join("state")),
    ];
    let outputs: Vec<_> = std::thread::scope(|scope| {
        let runs: Vec<_> = roots
            .iter()
            .zip(&guards)
            .map(|(root, guard)| {
                let scope_of_branch = guard.scope();
                scope.spawn(move || {
                    run_anchored(
                        PREFIX_SCRIPT,
                        root,
                        Some(scope_of_branch),
                        SandboxProfile::WorkspacePaths,
                    )
                })
            })
            .collect();
        runs.into_iter()
            .map(|run| run.join().expect("the branch thread completes"))
            .collect()
    });
    for ((guard, root), output) in guards.iter().zip(&roots).zip(outputs) {
        let report = guard.finish();
        assert_eq!(report.divergence, None, "{output:?}");
        assert_eq!(
            (report.served_calls, report.live_calls),
            (1, 1),
            "{report:?}"
        );
        assert_eq!(report.served_tool_calls, 1, "{report:?}");
        assert_eq!(report.applied_file_effects, 1, "{report:?}");
        let output = output.unwrap();
        // The pipeline sees the recorded workspace, never its own directory.
        assert!(
            output.contains(&format!("recorded live at {}", seed.display())),
            "{output}"
        );
        assert!(!output.contains(&root.display().to_string()), "{output}");
        assert_eq!(
            std::fs::read_to_string(root.join("made.txt")).unwrap(),
            "seed input\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("result.txt")).unwrap(),
            "live"
        );
    }
    assert_eq!(snapshot(seed), seed_before, "the seed must stay untouched");
    // Each branch wrote only its own root: the two hold the same files and
    // neither holds a file the other made under its name.
    assert_eq!(snapshot(&roots[0]), snapshot(&roots[1]));
}

/// The live tail runs commands the model wrote, one of them naming the
/// recorded workspace by absolute path.
fn live_command_script(seed: &Path) -> String {
    let seed = seed.display();
    format!(
        r#"
import {{ with_llm_script }} from "std/testing"
pipeline main(harness: Harness, task: unknown) {{
  const root = harness.agent.workspace_anchor("own-root").primary
  with_llm_script(harness.llm, [{{text: "prefix", model: "mock"}}, {{text: "live", model: "mock"}}], {{ ->
    harness.llm.call("anchored at ${{root}}", nil, {{provider: "mock", model: "mock"}})
    harness.llm.call("continue", nil, {{provider: "mock", model: "mock"}})
    const relative = harness.process.run({{program: "sh", args: ["-c", "echo relative > relative.txt"], cwd: root}})
    harness.stdio.println("relative exit=${{relative.exit_code}}")
    for command in ["echo leaked > {seed}/leaked.txt", "cat {seed}/input.txt"] {{
      try {{
        const absolute = harness.process.run({{program: "sh", args: ["-c", command], cwd: root}})
        harness.stdio.println("absolute exit=${{absolute.exit_code}} out=${{absolute.stdout}}")
      }} catch (error) {{
        harness.stdio.println("absolute refused: ${{error}}")
      }}
    }}
    harness.stdio.println("recorded")
  }})
}}
"#
    )
}

#[test]
fn a_live_command_naming_the_recorded_workspace_is_denied_in_a_relocated_branch() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    // The recording ran the commands in the recorded workspace, where the
    // absolute path was the model's own workspace.
    let recorded = record_seed(live_command_script);
    let seed = recorded.seed.clone();
    std::fs::remove_file(seed.join("leaked.txt")).unwrap();
    std::fs::remove_file(seed.join("relative.txt")).unwrap();
    let seed_before = snapshot(&seed);

    let own = dirs();
    let root = branch_root(&seed, &own);
    let guard = arm(&recorded, &root, &own.markers.join("state"));
    let output = run_anchored(
        &live_command_script(&seed),
        &root,
        Some(guard.scope()),
        SandboxProfile::OsHardened,
    );
    let report = guard.finish();
    assert_eq!(report.divergence, None, "{output:?}");
    assert_eq!((report.served_calls, report.live_calls), (1, 1));
    assert_eq!(snapshot(&seed), seed_before, "the seed must stay untouched");
    if harn_vm::process_sandbox::active_backend_filesystem_available() {
        let output = output.unwrap();
        assert!(output.contains("relative exit=0"), "{output}");
        assert_eq!(
            std::fs::read_to_string(root.join("relative.txt")).unwrap(),
            "relative\n"
        );
        assert!(!output.contains("seed input"), "{output}");
        assert_eq!(
            output.matches("absolute exit=0").count(),
            0,
            "a recorded absolute path must fail: {output}"
        );
        assert!(!root.join("leaked.txt").exists());
    } else {
        // No sandbox here holds the denial, so the branch refuses the command
        // instead of running it.
        let error = output.unwrap_err();
        assert!(
            error.contains("OS filesystem confinement keeps the recorded workspace"),
            "{error}"
        );
        assert!(!root.join("relative.txt").exists());
    }
}
