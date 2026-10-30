//! A branch replays a recorded run's prefix through the real agent loop
//! without re-running any of its effects: exec and tool hooks are served from
//! the tape or refused, never run, and durable state lands in the branch's own
//! state root, never the recording's.
// The scripts shell out to `touch`.
#![cfg(unix)]
#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use harn_vm::run_tape::{
    start_branch, start_recording, BranchPlan, BranchReport, BranchScope, EffectScan,
    RecordingLevel, TapeWorkspace, ToolEffectObserver, TAPE_FILE_NAME,
};
use harn_vm::testbench::tape::EventTape;
use harn_vm::value::VmError;

/// Recording and branch replay are process-global.
static SERIAL: Mutex<()> = Mutex::new(());

#[path = "run_tape_branch_vm/host_roots.rs"]
mod host_roots;

#[path = "run_tape_branch_vm/first_call.rs"]
mod first_call;

#[path = "run_tape_branch_vm/workspace.rs"]
mod workspace;

#[path = "run_tape_branch_vm/prompt_edits.rs"]
mod prompt_edits;

/// Records no file changes: every effect these scripts make lives outside the
/// workspace, so a served tool call has nothing to re-apply.
struct NoChanges;

impl ToolEffectObserver for NoChanges {
    fn begin(&self, _root: &Path, _call_id: &str) {}
    fn end(&self, _root: &Path, _call_id: &str) -> EffectScan {
        EffectScan {
            changes: vec![],
            incomplete: false,
        }
    }
}

fn quote(value: &Path) -> String {
    serde_json::to_string(&value.display().to_string()).unwrap()
}

/// Every run is pointed at the recording's session store, as a host reusing
/// its configuration would be; a branch must keep out of it on its own.
fn run(source: &str, workspace: &Path) -> Result<String, String> {
    name_session_store(workspace);
    run_in(source, workspace, None)
}

/// Start a run rooted at `root`: its cwd, and a write jail with no OS
/// confinement. These tests replay process runs rather than confine them,
/// and hosted macOS runners refuse sandbox-exec (SIP allows unrestricted
/// DTrace), so the default profile would reject every recorded exec there.
fn enter_run(root: &Path) {
    harn_vm::stdlib::process::set_thread_execution_context(Some(
        harn_vm::orchestration::RunExecutionRecord {
            cwd: Some(root.display().to_string()),
            project_root: Some(root.display().to_string()),
            ..Default::default()
        },
    ));
    harn_vm::orchestration::push_execution_policy(harn_vm::orchestration::CapabilityPolicy {
        workspace_roots: vec![root.display().to_string()],
        sandbox_profile: harn_vm::orchestration::SandboxProfile::WorkspacePaths,
        ..Default::default()
    });
}

fn leave_run() {
    harn_vm::orchestration::pop_execution_policy();
    harn_vm::stdlib::process::set_thread_execution_context(None);
}

/// Run `source` inside `branch`, when there is one.
fn run_in(source: &str, workspace: &Path, branch: Option<BranchScope>) -> Result<String, String> {
    run_at(
        source,
        workspace,
        branch,
        workspace.parent().expect("workspace has a parent"),
    )
}

fn run_at(
    source: &str,
    workspace: &Path,
    branch: Option<BranchScope>,
    root: &Path,
) -> Result<String, String> {
    let source = source.to_string();
    let workspace = workspace.to_path_buf();
    let root = root.to_path_buf();
    harn_vm::on_vm_stack(move || {
        harn_vm::reset_thread_local_state();
        let chunk = harn_vm::compile_source(&source)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let body = async move {
            // The run's write jail is the temp root holding the workspace and
            // the markers, not the test binary's cwd.
            enter_run(&root);
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

fn record(source: &str, tapes: &Path, workspace: &Path) -> String {
    record_with_roots(source, tapes, workspace, Vec::new())
}

fn record_with_roots(
    source: &str,
    tapes: &Path,
    workspace: &Path,
    state_roots: Vec<PathBuf>,
) -> String {
    let workspace_tape = TapeWorkspace {
        root: workspace.to_path_buf(),
        observer: Box::new(NoChanges),
        state_roots,
    };
    let guard = start_recording(tapes, RecordingLevel::Full, Some(workspace_tape))
        .unwrap()
        .expect("a full tape records");
    let output = run(source, workspace).expect("the recorded run succeeds");
    assert_eq!(guard.error(), None, "the tape wrote cleanly");
    drop(guard);
    output
}

/// The tape's tool-call records, for a failure message.
fn recorded_tool_calls(tapes: &Path) -> String {
    std::fs::read_to_string(tapes.join(TAPE_FILE_NAME))
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("\"tool_call\""))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Remove a marker the recorded run must have made.
fn remove_marker(marker: &Path, tapes: &Path) {
    if let Err(error) = std::fs::remove_file(marker) {
        panic!(
            "the recorded run made no {}: {error}\ntool calls: {}",
            marker.display(),
            recorded_tool_calls(tapes)
        );
    }
}

/// Point the host at the recording's session store for `workspace`, as it is
/// configured before a branch is armed.
fn name_session_store(workspace: &Path) {
    // SAFETY: tests here hold SERIAL, so no other thread reads the env.
    unsafe { std::env::set_var("HARN_SESSION_STORE_ROOT", recording_state(workspace)) };
}

/// The session store the host names for every run.
fn recording_state(workspace: &Path) -> PathBuf {
    workspace.parent().unwrap().join("record-state")
}

struct Branched {
    output: Result<String, String>,
    report: BranchReport,
    /// The branch's own state root.
    state: PathBuf,
}

fn branch(source: &str, tapes: &Path, workspace: &Path, k: u64) -> Branched {
    branch_with_roots(source, tapes, workspace, k, Vec::new())
}

fn branch_with_roots(
    source: &str,
    tapes: &Path,
    workspace: &Path,
    k: u64,
    host_state_roots: Vec<PathBuf>,
) -> Branched {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let state = workspace
        .parent()
        .unwrap()
        .join(format!("branch-state-{n}"));
    name_session_store(workspace);
    let guard = start_branch(BranchPlan {
        tape: EventTape::load(&tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: k,
        workspace_root: workspace.to_path_buf(),
        state_root: state.clone(),
        host_state_roots,
        prompt_edits: Vec::new(),
    })
    .unwrap();
    let output = run_in(source, workspace, Some(guard.scope()));
    let report = guard.finish();
    Branched {
        output,
        report,
        state: state.canonicalize().unwrap(),
    }
}

#[test]
fn input_reads_in_a_prefix_refuse_before_the_live_input_handler() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for invocation in [
        "harness.stdio.read_line()",
        "harness.stdio.read_stdin()",
        "harness.stdio.prompt(\"answer: \")",
        "harness.term.read_password(\"password: \")",
    ] {
        let dirs = dirs();
        let source = format!(
            r#"
import {{ with_llm_script }} from "std/testing"
pipeline main(harness: Harness, task: unknown) {{
  harness.testing.stdin_set("fixture input\n")
  harness.stdio.println("output before input")
  {invocation}
  harness.stdio.println("live input was consumed")
  with_llm_script(harness.llm, [{{text: "done", model: "mock"}}], {{ ->
    harness.llm.call("task", nil, {{provider: "mock", model: "mock"}})
  }})
}}
"#
        );
        let recorded = record(&source, &dirs.tapes, &dirs.workspace);
        assert!(
            recorded.contains("live input was consumed"),
            "{invocation}: {recorded}"
        );
        let replay = branch(&source, &dirs.tapes, &dirs.workspace, 1);
        let divergence = replay
            .report
            .divergence
            .expect("the prefix must refuse live input");
        assert_eq!(divergence.what, "refused effect");
        assert!(
            divergence.detail.contains("Stdio Read"),
            "{invocation}: {divergence:?}"
        );
        assert_eq!(replay.report.live_calls, 0);
    }
}

/// The runtime store a run under `state` saved.
fn store_at(state: &Path) -> serde_json::Value {
    let path = state.join(harn_vm::runtime_paths::STORE_FILE);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

struct Dirs {
    _root: tempfile::TempDir,
    tapes: PathBuf,
    workspace: PathBuf,
    markers: PathBuf,
}

fn dirs() -> Dirs {
    let root = tempfile::tempdir().unwrap();
    let tapes = root.path().join("tapes");
    let workspace = root.path().join("workspace");
    let markers = root.path().join("markers");
    for dir in [&tapes, &workspace, &markers] {
        std::fs::create_dir_all(dir).unwrap();
    }
    Dirs {
        _root: root,
        tapes,
        workspace,
        markers,
    }
}

/// One `mark` tool call, then done: the hook and the tool's exec each touch a
/// marker, and the tool writes the store.
fn mark_script(hook_marker: &Path, exec_marker: &Path) -> String {
    format!(
        r#"
import {{ agent_loop }} from "std/agent/loop"
import {{ llm_text, with_llm_script }} from "std/testing"

pipeline main(harness: Harness, task: unknown) {{
  const id = harness.random.uuid()
  const now = harness.clock.now_ms()
  harness.runtime.store_set("script_key", id)
  harness.tools.clear_hooks()
  harness.tools.register_hook(
    {{
      pattern: "mark",
      pre: {{ _event ->
        harness.fs.write_text({hook_marker}, "hook ran")
        return nil
      }},
    }},
  )
  let tools = tool_define(
    tool_registry(),
    "mark",
    "Touch the exec marker and write the store.",
    {{
      parameters: {{}},
      handler: {{ _args ->
        harness.process.run({{program: "touch", args: [{exec_marker}]}})
        harness.runtime.store_set("tool_key", "tool ran")
        return "marked"
      }},
    }},
  )
  const result = with_llm_script(
    harness.llm,
    [
      {{tool_calls: [{{id: "mark-1", name: "mark", arguments: {{}}}}]}},
      llm_text("<user_response>Marked.</user_response><done>##DONE##</done>"),
    ],
    {{ ->
      return agent_loop(
        harness,
        "mark run " + id + " at " + to_string(now),
        nil,
        {{
          provider: "mock",
          tools: tools,
          tool_format: "native",
          max_iterations: 3,
          loop_until_done: true,
          // Completion is not under test: a deterministic yes keeps the
          // terminal status independent of the runner's verifier facts.
          verify_completion: {{ _payload -> return true }},
          session_id: "branch-" + id,
        }},
      )
    }},
  )
  harness.tools.clear_hooks()
  harness.runtime.store_set("after_key", result.status)
  harness.stdio.log(result.status)
}}
"#,
        hook_marker = quote(hook_marker),
        exec_marker = quote(exec_marker),
    )
}

#[test]
fn a_branch_prefix_serves_exec_store_write_and_hook_without_running_them() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let source = mark_script(&hook_marker, &exec_marker);

    let recorded = record(&source, &dirs.tapes, &dirs.workspace);
    assert!(
        recorded.contains("[harn] done"),
        "recorded output: {recorded}\ntool calls: {}",
        recorded_tool_calls(&dirs.tapes)
    );
    let store_path = harn_vm::runtime_paths::store_path(&dirs.workspace);
    assert!(
        hook_marker.exists(),
        "the recorded run ran the pre-tool hook"
    );
    assert!(exec_marker.exists(), "the recorded run ran the tool's exec");
    let store: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&store_path).unwrap()).unwrap();
    assert_eq!(store["tool_key"], "tool ran", "store: {store}");
    assert!(store["script_key"].is_string(), "store: {store}");

    remove_marker(&hook_marker, &dirs.tapes);
    remove_marker(&exec_marker, &dirs.tapes);
    let sentinel = "{\"sentinel\": true}";
    std::fs::write(&store_path, sentinel).unwrap();

    let Branched {
        output,
        report,
        state,
    } = branch(&source, &dirs.tapes, &dirs.workspace, 2);
    let output = output.expect("the branched run succeeds");
    assert!(output.contains("[harn] done"), "branch output: {output}");
    assert_eq!(report.divergence, None, "report: {report:?}");
    assert_eq!(report.served_calls, 2, "report: {report:?}");
    assert_eq!(report.served_tool_calls, 1, "report: {report:?}");
    assert!(
        report.served_reads >= 2,
        "uuid and clock served: {report:?}"
    );
    assert_eq!(report.live_calls, 0, "report: {report:?}");
    assert!(!hook_marker.exists(), "the branch ran the pre-tool hook");
    assert!(!exec_marker.exists(), "the branch ran the tool's exec");
    assert_eq!(
        std::fs::read_to_string(&store_path).unwrap(),
        sentinel,
        "the branch wrote the recording's store file"
    );
    let branch_store = store_at(&state);
    assert_eq!(
        branch_store["script_key"], store["script_key"],
        "the prefix rebuilt the store in the branch's root: {branch_store}"
    );
}

/// The live tail answers from the mock script where the recording left off:
/// each served call consumed its scripted response, so live call 2 gets the
/// done text rather than a second `mark` tool call.
#[test]
fn a_served_tool_call_injects_the_reminders_its_recording_injected() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    // A runtime signal in the tool's result fires a post-tool review reminder
    // that the next model call's request carries.
    let source = mark_script(&hook_marker, &exec_marker).replace(
        r#"return "marked""#,
        r#"return "thread 'main' panicked at src/lib.rs:3:5: boom""#,
    );
    record(&source, &dirs.tapes, &dirs.workspace);
    let tape = std::fs::read_to_string(dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
    assert!(
        tape.contains("Grounded adversarial review"),
        "the recording injected the reminder"
    );

    let Branched { output, report, .. } = branch(&source, &dirs.tapes, &dirs.workspace, 2);
    let output = output.expect("the branched run succeeds");
    assert!(output.contains("[harn] done"), "branch output: {output}");
    assert_eq!(report.divergence, None, "report: {report:?}");
    assert_eq!(report.served_calls, 2, "report: {report:?}");
}

#[test]
fn a_branch_live_tail_continues_the_mock_script_after_the_served_prefix() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let source = mark_script(&hook_marker, &exec_marker);

    record(&source, &dirs.tapes, &dirs.workspace);
    remove_marker(&hook_marker, &dirs.tapes);
    remove_marker(&exec_marker, &dirs.tapes);
    let recorded_store =
        std::fs::read_to_string(harn_vm::runtime_paths::store_path(&dirs.workspace)).unwrap();

    let Branched {
        output,
        report,
        state,
    } = branch(&source, &dirs.tapes, &dirs.workspace, 1);
    let output = output.expect("the branched run succeeds");
    assert!(output.contains("[harn] done"), "branch output: {output}");
    assert_eq!(report.divergence, None, "report: {report:?}");
    assert_eq!(report.served_calls, 1, "report: {report:?}");
    assert_eq!(report.served_tool_calls, 1, "report: {report:?}");
    assert_eq!(report.live_calls, 1, "report: {report:?}");
    assert!(
        !hook_marker.exists(),
        "the branch re-ran the served tool's hook"
    );
    assert!(
        !exec_marker.exists(),
        "the branch re-ran the served tool's exec"
    );
    let store = store_at(&state);
    assert_eq!(
        store["after_key"], "done",
        "a live-tail write saves: {store}"
    );
    assert!(
        store["script_key"].is_string(),
        "prefix writes carry over: {store}"
    );
    assert_eq!(
        std::fs::read_to_string(harn_vm::runtime_paths::store_path(&dirs.workspace)).unwrap(),
        recorded_store,
        "the branch changed the recording's store"
    );
}

#[test]
fn a_script_level_exec_in_the_prefix_is_served_not_run() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let exec_marker = dirs.markers.join("script-exec-marker");
    let source = format!(
        r#"
import {{ agent_loop }} from "std/agent/loop"
import {{ llm_text, with_llm_script }} from "std/testing"

pipeline main(harness: Harness, task: unknown) {{
  const id = harness.random.uuid()
  harness.process.run({{program: "touch", args: [{exec_marker}]}})
  const result = with_llm_script(
    harness.llm,
    [llm_text("<user_response>Done.</user_response><done>##DONE##</done>")],
    {{ ->
      return agent_loop(
        harness,
        "finish run " + id,
        nil,
        {{provider: "mock", max_iterations: 1, loop_until_done: true, session_id: "branch-" + id}},
      )
    }},
  )
  harness.stdio.log(result.status)
}}
"#,
        exec_marker = quote(&exec_marker),
    );

    let recorded = record(&source, &dirs.tapes, &dirs.workspace);
    assert!(
        recorded.contains("[harn] done"),
        "recorded output: {recorded}\ntool calls: {}",
        recorded_tool_calls(&dirs.tapes)
    );
    assert!(exec_marker.exists(), "the recorded run ran the exec");
    remove_marker(&exec_marker, &dirs.tapes);

    let Branched { output, report, .. } = branch(&source, &dirs.tapes, &dirs.workspace, 1);
    assert!(
        output
            .as_ref()
            .is_ok_and(|output| output.contains("[harn] done")),
        "{output:?}"
    );
    assert!(
        !exec_marker.exists(),
        "the branch ran the script-level exec"
    );
    assert_eq!(report.served_calls, 1, "report: {report:?}");
    assert_eq!(report.served_tool_calls, 1, "report: {report:?}");
    assert_eq!(report.divergence, None, "report: {report:?}");

    let other_marker = dirs.markers.join("unrecorded-exec");
    let changed = source.replace(&quote(&exec_marker), &quote(&other_marker));
    let Branched { output, report, .. } = branch(&changed, &dirs.tapes, &dirs.workspace, 1);
    assert!(
        output.is_err(),
        "an unrecorded command must fail: {output:?}"
    );
    assert!(!other_marker.exists());
    let divergence = report.divergence.expect("an unrecorded command diverges");
    assert_eq!(divergence.what, "tool call");
    assert!(divergence.detail.contains("process:process.exec"));
    assert_eq!(report.live_calls, 0);
}

/// One `mark` tool call, then done, with a post-turn session reset.
fn reset_script(hook_marker: &Path, exec_marker: &Path) -> String {
    mark_script(hook_marker, exec_marker).replace(
        "          session_id: \"branch-\" + id,",
        "          session_id: \"branch-\" + id,\n          post_turn_callback: { _turn -> \n            harness.agent.reset(\"branch-\" + id)\n            return nil\n          },",
    )
}

/// Every event the durable session stores under `root` hold for sessions whose
/// id starts with `session`, as `kind` joined with the payload's text.
fn durable_events(root: &Path, session: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path
                .extension()
                .is_some_and(|ext| ext == "sqlite" || ext == "db")
            {
                found.push(path);
            }
        }
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        use harn_session_store::SessionStore;
        let mut events = Vec::new();
        for path in found {
            let store = harn_session_store::SqliteSessionStore::open(&path).unwrap();
            for meta in store.list(Default::default()).await.unwrap() {
                if !meta.id.starts_with(session) {
                    continue;
                }
                for event in store.read_all(&meta.id).await.unwrap() {
                    events.push(format!("{:?} {}", event.kind, event.payload));
                }
            }
        }
        events
    })
}

/// Session resets the durable stores under `root` hold: a reset lands as an
/// empty compaction.
fn durable_resets(root: &Path) -> usize {
    durable_events(root, "")
        .iter()
        .filter(|event| event.starts_with("Compaction ") && event.contains("\"messages\":[]"))
        .count()
}

/// Replaying the prefix rebuilds the session's history in the branch's own
/// store exactly once, and never touches the recording's, though the host
/// still names the recording's store and the session id is the recorded one.
#[test]
fn a_session_reset_in_the_prefix_lands_once_in_the_branch_store_only() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let source = reset_script(&hook_marker, &exec_marker);
    record(&source, &dirs.tapes, &dirs.workspace);
    let recording = recording_state(&dirs.workspace);
    assert_eq!(durable_resets(&recording), 2, "one reset per turn");

    for k in [2, 1] {
        let Branched {
            output,
            report,
            state,
        } = branch(&source, &dirs.tapes, &dirs.workspace, k);
        output.expect("the branched run succeeds");
        assert_eq!(report.divergence, None, "K={k} report: {report:?}");
        assert_eq!(report.live_calls, 2 - k, "K={k} report: {report:?}");
        assert_eq!(
            durable_resets(&state),
            2,
            "K={k}: the branch's history holds each reset once"
        );
        assert_eq!(
            durable_resets(&recording),
            2,
            "K={k}: the branch wrote into the recording's session store"
        );
    }
}

/// A branch handed the recording's own session store as its state root is
/// refused before anything runs.
#[test]
fn a_branch_refuses_the_recordings_state_root() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    record(
        &reset_script(&hook_marker, &exec_marker),
        &dirs.tapes,
        &dirs.workspace,
    );
    let error = start_branch(BranchPlan {
        tape: EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 1,
        workspace_root: dirs.workspace.clone(),
        state_root: recording_state(&dirs.workspace),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    })
    .err()
    .expect("the recording's state root is refused");
    assert!(error.contains("overlaps the recording's"), "{error}");
    // Inside a recording tree and holding one are refused the same way.
    for state_root in [
        recording_state(&dirs.workspace).join("nested"),
        dirs.workspace.join("branch-state"),
        dirs.workspace.parent().unwrap().to_path_buf(),
    ] {
        let error = start_branch(BranchPlan {
            tape: EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap(),
            live_after_calls: 1,
            workspace_root: dirs.workspace.clone(),
            state_root: state_root.clone(),
            host_state_roots: Vec::new(),
            prompt_edits: Vec::new(),
        })
        .err()
        .unwrap_or_else(|| panic!("{} is refused", state_root.display()));
        assert!(error.contains("overlaps the recording's"), "{error}");
    }
}

/// Two agent loops, each with its own session; the first finishes inside the
/// prefix, so its whole history must reach the branch's store.
#[test]
fn a_loop_that_finishes_in_the_prefix_keeps_its_history() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let source = r#"
import { agent_loop } from "std/agent/loop"
import { llm_text, with_llm_script } from "std/testing"

pipeline main(harness: Harness, task: unknown) {
  const id = harness.random.uuid()
  const done = llm_text("<user_response>Done.</user_response><done>##DONE##</done>")
  let statuses = []
  for name in ["first", "second"] {
    const result = with_llm_script(
      harness.llm,
      [done],
      { ->
        return agent_loop(
          harness,
          name + " loop of " + id,
          nil,
          {
            provider: "mock",
            max_iterations: 1,
            loop_until_done: true,
            verify_completion: { _payload -> return true },
            session_id: name + "-" + id,
          },
        )
      },
    )
    statuses.push(result.status)
  }
  harness.stdio.log(join(statuses, ","))
}
"#;
    let recorded = record(source, &dirs.tapes, &dirs.workspace);
    assert!(
        recorded.contains("done,done"),
        "recorded output: {recorded}"
    );
    let first = durable_events(&recording_state(&dirs.workspace), "first-");
    assert!(
        !first.is_empty(),
        "the recording kept the first loop's history"
    );

    let Branched {
        output,
        report,
        state,
    } = branch(source, &dirs.tapes, &dirs.workspace, 1);
    output.expect("the branched run succeeds");
    assert_eq!(report.divergence, None, "report: {report:?}");
    assert_eq!(report.live_calls, 1, "report: {report:?}");
    assert_eq!(
        durable_events(&state, "first-").len(),
        first.len(),
        "the branch lost the first loop's history"
    );
}

/// Prefix store writes reach the branch's store even when the live tail never
/// writes the store itself, and the recording's store keeps what it had.
#[test]
fn prefix_store_writes_reach_the_branch_store() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let source = mark_script(&hook_marker, &exec_marker).replace(
        "  harness.runtime.store_set(\"after_key\", result.status)\n",
        "",
    );
    assert!(
        !source.contains("after_key"),
        "the live tail must not write"
    );
    record(&source, &dirs.tapes, &dirs.workspace);
    let recorded_store =
        std::fs::read_to_string(harn_vm::runtime_paths::store_path(&dirs.workspace)).unwrap();

    let Branched {
        output,
        report,
        state,
    } = branch(&source, &dirs.tapes, &dirs.workspace, 1);
    output.expect("the branched run succeeds");
    assert_eq!(report.live_calls, 1, "report: {report:?}");
    let store = store_at(&state);
    assert!(
        store["script_key"].is_string(),
        "the prefix write was saved: {store}"
    );
    assert_eq!(
        std::fs::read_to_string(harn_vm::runtime_paths::store_path(&dirs.workspace)).unwrap(),
        recorded_store,
        "the branch changed the recording's store"
    );
}

/// `reset_script` with every session opened at an explicit store `root`.
fn rooted_reset_script(hook_marker: &Path, exec_marker: &Path, root: &Path) -> String {
    reset_script(hook_marker, exec_marker).replace(
        "          session_id: \"branch-\" + id,",
        &format!(
            "          session_id: \"branch-\" + id,\n          root: {},",
            quote(root)
        ),
    )
}

/// A session store named explicitly inside a tree the recording owns, at the
/// recording's own root or nested under it, still lands in the branch's
/// state: each branch holds the resets once and the recording keeps two.
#[test]
fn an_explicit_session_root_inside_the_recording_lands_in_the_branch() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for nested in ["", "nested"] {
        let dirs = dirs();
        let hook_marker = dirs.markers.join("hook-marker");
        let exec_marker = dirs.markers.join("exec-marker");
        let root = recording_state(&dirs.workspace).join(nested);
        let source = rooted_reset_script(&hook_marker, &exec_marker, &root);
        record(&source, &dirs.tapes, &dirs.workspace);
        let recording = recording_state(&dirs.workspace);
        assert_eq!(
            durable_resets(&recording),
            2,
            "{nested:?}: one reset per turn"
        );
        for k in [2, 1] {
            let Branched {
                output,
                report,
                state,
            } = branch(&source, &dirs.tapes, &dirs.workspace, k);
            output.expect("the branched run succeeds");
            assert_eq!(report.divergence, None, "{nested:?} K={k}: {report:?}");
            assert_eq!(
                durable_resets(&state),
                2,
                "{nested:?} K={k}: branch history"
            );
            assert_eq!(
                durable_resets(&recording),
                2,
                "{nested:?} K={k}: the branch wrote into the recording's tree"
            );
        }
    }
}

/// The newest event on every topic of `log`.
async fn log_heads(log: &harn_vm::event_log::AnyEventLog) -> Vec<String> {
    use harn_vm::event_log::EventLog;
    let mut heads = Vec::new();
    for topic in log.topics().await.unwrap() {
        heads.push(format!("{topic:?} {:?}", log.latest(&topic).await.unwrap()));
    }
    heads
}

/// Bytes of every file under `root` but the event log, whose SQLite file
/// changes on checkpoint without a write, by path.
fn tree_bytes(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut files = std::collections::BTreeMap::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("events.sqlite"))
            {
                files.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    files
}

/// A host that builds its VM, state builtins and event log at the
/// recording's root before arming the branch still keeps the branch out of
/// it: every durable path resolves when it is used.
#[test]
fn state_built_before_the_branch_is_armed_stays_out_of_the_recording() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let source = mark_script(&hook_marker, &exec_marker);
    record(&source, &dirs.tapes, &dirs.workspace);
    let recording = recording_state(&dirs.workspace);
    let state = dirs.workspace.parent().unwrap().join("branch-state");
    let plan = BranchPlan {
        tape: EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 2,
        workspace_root: dirs.workspace.clone(),
        state_root: state.clone(),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    };
    let workspace = dirs.workspace;
    let (output, report, before, after) = harn_vm::on_vm_stack(move || {
        harn_vm::reset_thread_local_state();
        let chunk = harn_vm::compile_source(&source).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(harn_clock::test_support::with_local_tasks(async move {
            let root = workspace.parent().unwrap();
            enter_run(root);
            let mut vm = harn_vm::new_vm();
            harn_runtime::register_vm_stdlib(&mut vm);
            harn_vm::register_persistent_state_builtins_at_root(
                &mut vm,
                &workspace,
                harn_vm::PersistentStateRoot::new(&recording),
                "branch",
            );
            let log = harn_vm::event_log::install_default_at_state_root(&recording).unwrap();
            let heads = log_heads(&log).await;
            let before = (tree_bytes(&recording), tree_bytes(&workspace));
            let guard = start_branch(plan).unwrap();
            let output = guard
                .scope()
                .run(Box::pin(vm.execute(&chunk)))
                .await
                .map(|_| vm.output().to_string())
                .map_err(|error| format!("{error:?}"));
            harn_vm::event_log::reset_active_event_log();
            leave_run();
            assert_eq!(
                log_heads(&log).await,
                heads,
                "the branch wrote the recording's log"
            );
            let after = (tree_bytes(&recording), tree_bytes(&workspace));
            (output, guard.finish(), before, after)
        }))
    });
    output.expect("the branched run succeeds");
    assert_eq!(report.divergence, None, "report: {report:?}");
    // Neither the recording's state nor its workspace, which also holds run
    // records, changes.
    let changed: Vec<_> = [(&before.0, &after.0), (&before.1, &after.1)]
        .into_iter()
        .flat_map(|(before, after)| {
            after
                .iter()
                .filter(|(path, bytes)| before.get(*path) != Some(*bytes))
                .map(|(path, _)| path.display().to_string())
        })
        .collect();
    assert!(
        changed.is_empty(),
        "the branch changed the recording's {changed:?}"
    );
    let state = state.canonicalize().unwrap();
    assert_eq!(
        store_at(&state)["after_key"],
        "done",
        "the branch's own store"
    );
    assert!(
        harn_vm::runtime_paths::event_log_sqlite_path_at_state_root(&state).exists(),
        "the branch opened its own event log"
    );
}

/// Two sessions share one host thread and only one branches: the other
/// keeps its own state root, its exec runs and its clock reads live, and
/// the branch serves its prefix as if it ran alone.
#[test]
fn a_branch_leaves_another_session_in_the_same_host_untouched() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let other_marker = dirs.markers.join("other-marker");
    let source = mark_script(&hook_marker, &exec_marker);
    record(&source, &dirs.tapes, &dirs.workspace);
    remove_marker(&hook_marker, &dirs.tapes);
    remove_marker(&exec_marker, &dirs.tapes);
    let other_workspace = dirs.workspace.parent().unwrap().join("other");
    std::fs::create_dir_all(&other_workspace).unwrap();
    let other_source = format!(
        r#"
pipeline main(harness: Harness, task: unknown) {{
  const now = harness.clock.now_ms()
  harness.process.run({{program: "touch", args: [{marker}]}})
  harness.runtime.store_set("other_key", to_string(now))
}}
"#,
        marker = quote(&other_marker)
    );
    let state = dirs.workspace.parent().unwrap().join("branch-state");
    let guard = start_branch(BranchPlan {
        tape: EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 2,
        workspace_root: dirs.workspace.clone(),
        state_root: state.clone(),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    })
    .unwrap();
    let scope = guard.scope();
    let (workspace, other) = (dirs.workspace, other_workspace.clone());
    let (branched, other_run) = harn_vm::on_vm_stack(move || {
        harn_vm::reset_thread_local_state();
        let chunk = harn_vm::compile_source(&source).unwrap();
        let other_chunk = harn_vm::compile_source(&other_source).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(harn_clock::test_support::with_local_tasks(async move {
            let root = workspace.parent().unwrap();
            enter_run(root);
            let mut vm = harn_vm::new_vm();
            harn_runtime::register_vm_stdlib(&mut vm);
            harn_vm::register_store_builtins(&mut vm, &workspace);
            let mut other_vm = harn_vm::new_vm();
            harn_runtime::register_vm_stdlib(&mut other_vm);
            harn_vm::register_store_builtins(&mut other_vm, &other);
            let (branched, other_run) = tokio::join!(
                Box::pin(scope.run(vm.execute(&chunk))),
                Box::pin(other_vm.execute(&other_chunk))
            );
            leave_run();
            (
                branched.map(|_| ()).map_err(|error| format!("{error:?}")),
                other_run.map(|_| ()).map_err(|error| format!("{error:?}")),
            )
        }))
    });
    let report = guard.finish();
    branched.expect("the branched run succeeds");
    other_run.expect("the other session runs live");
    assert_eq!(report.divergence, None, "report: {report:?}");
    assert_eq!(report.served_calls, 2, "report: {report:?}");
    assert!(!exec_marker.exists(), "the branch's prefix exec ran");
    assert!(!hook_marker.exists(), "the branch's prefix hook ran");
    assert!(
        other_marker.exists(),
        "the other session's exec was refused"
    );
    let other_store = harn_vm::runtime_paths::store_path(&other_workspace);
    let other_store: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&other_store).unwrap()).unwrap();
    assert!(other_store["other_key"].is_string(), "{other_store}");
    let branch_store = store_at(&state.canonicalize().unwrap());
    assert!(branch_store.get("other_key").is_none(), "{branch_store}");
}

/// A tape branches from another machine's layout: a different workspace
/// path and home directory leave every request digest unchanged.
#[test]
fn a_tape_recorded_under_one_root_branches_under_another() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let recorded = dirs();
    let hook_marker = recorded.markers.join("hook-marker");
    let exec_marker = recorded.markers.join("exec-marker");
    let source = mark_script(&hook_marker, &exec_marker);
    record(&source, &recorded.tapes, &recorded.workspace);
    let elsewhere = dirs();
    let home = elsewhere.markers.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let previous_home = std::env::var_os("HOME");
    // SAFETY: tests here hold SERIAL, so no other thread reads the env.
    unsafe { std::env::set_var("HOME", &home) };
    let Branched { output, report, .. } = branch(&source, &recorded.tapes, &elsewhere.workspace, 2);
    match previous_home {
        // SAFETY: as above.
        Some(previous) => unsafe { std::env::set_var("HOME", previous) },
        None => unsafe { std::env::remove_var("HOME") },
    }
    output.expect("the branched run succeeds");
    assert_eq!(report.divergence, None, "report: {report:?}");
    assert_eq!(report.served_calls, 2, "report: {report:?}");
}

/// A pre-loop script whose `setup` statement runs inside a spawned task, then
/// one model call.
fn spawned_script(setup: &str) -> String {
    format!(
        r#"
import {{ agent_loop }} from "std/agent/loop"
import {{ llm_text, with_llm_script }} from "std/testing"

pipeline main(harness: Harness, task: unknown) {{
  const id = harness.random.uuid()
  {setup}
  const result = with_llm_script(
    harness.llm,
    [llm_text("<user_response>Done.</user_response><done>##DONE##</done>")],
    {{ ->
      return agent_loop(
        harness,
        "finish run " + id,
        nil,
        {{provider: "mock", max_iterations: 1, loop_until_done: true, session_id: "branch-" + id}},
      )
    }},
  )
  harness.stdio.log(result.status)
}}
"#
    )
}

/// Record `source`, then branch it at K=1: the spawned task must belong to the
/// branch, so its exec is served rather than run and its store write never
/// reaches the recording's store.
fn assert_spawned_task_stays_in_the_branch(source: &str, marker: &Path) {
    let dirs = dirs();
    let recorded = record(source, &dirs.tapes, &dirs.workspace);
    assert!(
        recorded.contains("[harn] done"),
        "recorded output: {recorded}\ntool calls: {}",
        recorded_tool_calls(&dirs.tapes)
    );
    assert!(marker.exists(), "the recorded run ran the spawned exec");
    std::fs::remove_file(marker).unwrap();
    let recorded_store =
        std::fs::read_to_string(harn_vm::runtime_paths::store_path(&dirs.workspace)).unwrap();

    let Branched { output, report, .. } = branch(source, &dirs.tapes, &dirs.workspace, 1);
    assert!(
        !marker.exists(),
        "the branch re-ran a spawned task's exec: {output:?}"
    );
    assert!(
        output
            .as_ref()
            .is_ok_and(|output| output.contains("[harn] done")),
        "{output:?}"
    );
    assert_eq!(report.divergence, None, "{report:?}");
    assert_eq!(report.served_tool_calls, 1, "{report:?}");
    assert_eq!(
        std::fs::read_to_string(harn_vm::runtime_paths::store_path(&dirs.workspace)).unwrap(),
        recorded_store,
        "a spawned task wrote the recording's store"
    );
}

#[test]
fn a_parallel_race_task_in_the_prefix_belongs_to_the_branch() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let marker = tempfile::tempdir().unwrap().keep().join("race-marker");
    let setup = format!(
        r#"try {{
    harness.runtime.parallel_race([1], {{ _x ->
      harness.process.run({{program: "touch", args: [{marker}]}})
      harness.runtime.store_set("race_key", "raced")
      return "raced"
    }})
  }}"#,
        marker = quote(&marker),
    );
    assert_spawned_task_stays_in_the_branch(&spawned_script(&setup), &marker);
}

/// A supervisor child started in the branch's live tail writes the branch's
/// store, not the recording's: the child task carries the branch scope.
#[test]
fn a_supervisor_child_in_the_live_tail_writes_the_branch_store() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    // Two LLM calls come first; at K=1 the second goes live, so the
    // supervisor starts in the live tail.
    let source = r#"
import { agent_loop } from "std/agent/loop"
import { llm_text, with_llm_script } from "std/testing"

pipeline main(harness: Harness, task: unknown) {
  const id = harness.random.uuid()
  const result = with_llm_script(
    harness.llm,
    [llm_text("<user_response>Done.</user_response><done>##DONE##</done>")],
    { ->
      return agent_loop(
        harness,
        "finish run " + id,
        nil,
        {provider: "mock", max_iterations: 1, loop_until_done: true, session_id: "branch-" + id},
      )
    },
  )
  // At K=1 this second call is the first live one.
  with_llm_script(
    harness.llm,
    [llm_text("<user_response>Again.</user_response><done>##DONE##</done>")],
    { ->
      return agent_loop(
        harness,
        "again " + id,
        nil,
        {provider: "mock", max_iterations: 1, loop_until_done: true, session_id: "again-" + id},
      )
    },
  )
  const done = harness.runtime.channel("child-done-" + id, 1)
  const sup = harness.runtime.supervisor_start({
    name: "branch-sup",
    strategy: "one_for_one",
    children: [{
      name: "writer",
      restart: "never",
      task: { _ctx ->
        harness.runtime.store_set("child_key", harness.random.uuid())
        harness.runtime.send(done, "done")
        "done"
      },
    }],
  })
  deadline 5s { harness.runtime.receive(done) }
  harness.runtime.supervisor_stop(sup, 2s)
  harness.stdio.log(result.status)
}
"#
    .to_string();
    let recorded = record(&source, &dirs.tapes, &dirs.workspace);
    assert!(
        recorded.contains("[harn] done"),
        "recorded output: {recorded}"
    );
    let store_path = harn_vm::runtime_paths::store_path(&dirs.workspace);
    let recorded_store = std::fs::read_to_string(&store_path).unwrap();
    let recorded_child: serde_json::Value = serde_json::from_str(&recorded_store).unwrap();

    let Branched {
        output,
        report,
        state,
    } = branch(&source, &dirs.tapes, &dirs.workspace, 1);
    let output = output.expect("the live branch succeeds");
    assert!(output.contains("[harn] done"), "branch output: {output}");
    assert_eq!(report.divergence, None, "report: {report:?}");
    let branch_store = store_at(&state);
    assert!(
        branch_store["child_key"].is_string(),
        "the child's write reached the branch store: {branch_store}"
    );
    assert_ne!(
        branch_store["child_key"], recorded_child["child_key"],
        "the live child wrote its own run's id: {branch_store}"
    );
    assert_eq!(
        std::fs::read_to_string(&store_path).unwrap(),
        recorded_store,
        "the supervisor child wrote the recording's store"
    );
}

/// A host that names its session store only after the branch starts, such as
/// a fresh home per run, still gets that store inside the branch.
#[test]
fn a_session_store_named_after_the_branch_starts_lands_in_the_branch() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let source = mark_script(&hook_marker, &exec_marker);
    record(&source, &dirs.tapes, &dirs.workspace);
    remove_marker(&hook_marker, &dirs.tapes);
    remove_marker(&exec_marker, &dirs.tapes);

    let state = dirs.workspace.parent().unwrap().join("late-branch-state");
    let late_home = dirs.workspace.parent().unwrap().join("late-home");
    name_session_store(&dirs.workspace);
    let guard = start_branch(BranchPlan {
        tape: EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: 1,
        workspace_root: dirs.workspace.clone(),
        state_root: state.clone(),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    })
    .unwrap();
    // SAFETY: tests here hold SERIAL, so no other thread reads the env.
    unsafe { std::env::set_var("HARN_SESSION_STORE_ROOT", &late_home) };
    let output = run_in(&source, &dirs.workspace, Some(guard.scope()));
    let report = guard.finish();
    output.expect("the branched run succeeds");
    assert_eq!(report.divergence, None, "report: {report:?}");
    assert!(
        !late_home.exists(),
        "the branch wrote the late-named session store in place"
    );
    assert!(
        tree_bytes(&state)
            .keys()
            .any(|path| path.to_string_lossy().contains("session-store")),
        "the session store landed in the branch: {:?}",
        tree_bytes(&state).keys().collect::<Vec<_>>()
    );
}

#[test]
fn the_llm_transcript_inside_the_recording_lands_in_the_branch() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let source = mark_script(&hook_marker, &exec_marker);
    let transcripts = dirs.workspace.join("transcripts");
    let recorded = transcripts.join("llm_transcript.jsonl");
    // SAFETY: tests here hold SERIAL, so no other thread reads the env.
    unsafe { std::env::set_var("HARN_LLM_TRANSCRIPT_DIR", &transcripts) };
    record(&source, &dirs.tapes, &dirs.workspace);
    let before = std::fs::read(&recorded).expect("the recording wrote its transcript");
    remove_marker(&hook_marker, &dirs.tapes);
    remove_marker(&exec_marker, &dirs.tapes);
    let branched = branch(&source, &dirs.tapes, &dirs.workspace, 1);
    unsafe { std::env::remove_var("HARN_LLM_TRANSCRIPT_DIR") };
    branched.output.expect("the branched run succeeds");
    let report = branched.report;
    assert_eq!(report.divergence, None, "report: {report:?}");
    assert_eq!(
        std::fs::read(&recorded).unwrap(),
        before,
        "the branch appended to the recording's transcript"
    );
    assert!(
        tree_bytes(&branched.state)
            .keys()
            .any(|path| path.ends_with("llm_transcript.jsonl")),
        "the transcript landed in the branch"
    );
}

/// `mark_script` whose tool handler also runs `change` against its session,
/// bound as `sid`, inside the dispatch.
fn transcript_change_script(hook_marker: &Path, exec_marker: &Path, change: &str) -> String {
    let source = mark_script(hook_marker, exec_marker)
        .replace(
            "  let tools = tool_define(",
            "  const sid = \"branch-\" + id\n  let tools = tool_define(",
        )
        .replace(
            "        return \"marked\"\n",
            &format!("        {change}\n        return \"marked\"\n"),
        );
    assert!(source.contains(change));
    source
}

/// Each transcript writer a tool dispatch reaches is taped at the one owner,
/// so a served call leaves the next request exactly as the recording sent it.
#[test]
fn a_served_tool_call_replays_every_transcript_writer() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let host_event = r#"harness.agent.inject_host_event(sid, {kind: "host_tool_result", delivery: "immediate", payload: {tool_name: "note", raw_output: "hook host note"}, provenance: {initiator: "host_auto", source: "test", host: "tui", ts_ms: 1}})"#;
    for (writer, change) in [
        ("inject_host_event", host_event),
        ("trim", "harness.agent.trim(sid, 1)"),
    ] {
        let dirs = dirs();
        let hook_marker = dirs.markers.join("hook-marker");
        let exec_marker = dirs.markers.join("exec-marker");
        let source = transcript_change_script(&hook_marker, &exec_marker, change);
        record(&source, &dirs.tapes, &dirs.workspace);
        let tape = std::fs::read_to_string(dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
        assert!(
            tape.contains("\"transcript_changes\""),
            "{writer}: the dispatch's change was not taped"
        );
        remove_marker(&hook_marker, &dirs.tapes);
        remove_marker(&exec_marker, &dirs.tapes);

        let Branched { output, report, .. } = branch(&source, &dirs.tapes, &dirs.workspace, 2);
        output.expect("the branched run succeeds");
        assert_eq!(report.divergence, None, "{writer}: {report:?}");
        assert_eq!(report.served_tool_calls, 1, "{writer}: {report:?}");
        assert!(!exec_marker.exists(), "{writer}: the branch ran the tool");
    }
}

/// A generator body runs as its own task; in the prefix it belongs to the
/// branch like any spawned task.
#[test]
fn a_generator_body_in_the_prefix_belongs_to_the_branch() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let marker = tempfile::tempdir().unwrap().keep().join("gen-marker");
    let setup = format!(
        r#"fn toucher(h: Harness) {{
    try {{
      h.process.run({{program: "touch", args: [{marker}]}})
      h.runtime.store_set("gen_key", "ran")
    }}
    yield 1
  }}
  for _v in toucher(harness) {{
    harness.stdio.log("gen yielded")
  }}"#,
        marker = quote(&marker),
    );
    assert_spawned_task_stays_in_the_branch(&spawned_script(&setup), &marker);
}

/// A pre-tool hook's reminder is a transcript injection inside the dispatch:
/// a served call replays it, so the next request matches the recording.
#[test]
fn a_served_tool_call_replays_its_pre_hook_reminder() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let source = mark_script(&hook_marker, &exec_marker).replace(
        "        return nil\n",
        "        return {reminder: {body: \"pre tool reminder\", tags: [\"tool\"]}}\n",
    );
    assert!(source.contains("pre tool reminder"));
    record(&source, &dirs.tapes, &dirs.workspace);
    remove_marker(&hook_marker, &dirs.tapes);
    remove_marker(&exec_marker, &dirs.tapes);

    let Branched { output, report, .. } = branch(&source, &dirs.tapes, &dirs.workspace, 2);
    output.expect("the branched run succeeds");
    assert_eq!(report.divergence, None, "report: {report:?}");
    assert_eq!(report.served_tool_calls, 1, "report: {report:?}");
    assert!(
        !hook_marker.exists(),
        "the branch re-ran the served tool's hook"
    );
}
