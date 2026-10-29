//! A branch replays a recorded run's prefix through the real agent loop
//! without re-running any of its effects: exec and tool hooks are served from
//! the tape or refused, never run, and durable state lands in the branch's own
//! state root, never the recording's.
// The scripts shell out to `touch`.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use harn_vm::run_tape::{
    start_branch, start_recording, BranchPlan, BranchReport, EffectScan, RecordingLevel,
    TapeWorkspace, ToolEffectObserver, TAPE_FILE_NAME,
};
use harn_vm::testbench::tape::EventTape;
use harn_vm::value::VmError;

/// Recording and branch replay are process-global.
static SERIAL: Mutex<()> = Mutex::new(());

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
    let state_dir = recording_state(workspace);
    // SAFETY: tests here hold SERIAL, so no other thread reads the env.
    unsafe { std::env::set_var("HARN_SESSION_STORE_ROOT", &state_dir) };
    let source = source.to_string();
    let workspace = workspace.to_path_buf();
    harn_vm::on_vm_stack(move || {
        harn_vm::reset_thread_local_state();
        let chunk = harn_vm::compile_source(&source)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        rt.block_on(harn_clock::test_support::with_local_tasks(async move {
            // The run's write jail is the temp root holding the workspace and
            // the markers, not the test binary's cwd.
            let root = workspace.parent().expect("workspace has a parent");
            harn_vm::stdlib::process::set_thread_execution_context(Some(
                harn_vm::orchestration::RunExecutionRecord {
                    cwd: Some(root.display().to_string()),
                    project_root: Some(root.display().to_string()),
                    ..Default::default()
                },
            ));
            let mut vm = harn_vm::new_vm();
            harn_runtime::register_vm_stdlib(&mut vm);
            harn_vm::register_store_builtins(&mut vm, &workspace);
            let result = vm
                .execute(&chunk)
                .await
                .map_err(|error: VmError| format!("{error:?}"));
            harn_vm::stdlib::process::set_thread_execution_context(None);
            result?;
            Ok(vm.output().to_string())
        }))
    })
}

fn record(source: &str, tapes: &Path, workspace: &Path) -> String {
    let workspace_tape = TapeWorkspace {
        root: workspace.to_path_buf(),
        observer: Box::new(NoChanges),
    };
    let guard = start_recording(tapes, RecordingLevel::Full, Some(workspace_tape))
        .unwrap()
        .expect("a full tape records");
    let output = run(source, workspace).expect("the recorded run succeeds");
    assert_eq!(guard.error(), None, "the tape wrote cleanly");
    drop(guard);
    output
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
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let state = workspace
        .parent()
        .unwrap()
        .join(format!("branch-state-{n}"));
    let guard = start_branch(BranchPlan {
        tape: EventTape::load(&tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: k,
        workspace_root: workspace.to_path_buf(),
        state_root: state.clone(),
    })
    .unwrap();
    let output = run(source, workspace);
    let report = guard.finish();
    Branched {
        output,
        report,
        state: state.canonicalize().unwrap(),
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
        "recorded output: {recorded}"
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

    std::fs::remove_file(&hook_marker).unwrap();
    std::fs::remove_file(&exec_marker).unwrap();
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
fn a_branch_live_tail_continues_the_mock_script_after_the_served_prefix() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let source = mark_script(&hook_marker, &exec_marker);

    record(&source, &dirs.tapes, &dirs.workspace);
    std::fs::remove_file(&hook_marker).unwrap();
    std::fs::remove_file(&exec_marker).unwrap();
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
fn a_script_level_exec_in_the_prefix_is_refused_not_run() {
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
        "recorded output: {recorded}"
    );
    assert!(exec_marker.exists(), "the recorded run ran the exec");
    std::fs::remove_file(&exec_marker).unwrap();

    let Branched { output, report, .. } = branch(&source, &dirs.tapes, &dirs.workspace, 1);
    assert!(output.is_err(), "a refused exec fails the run: {output:?}");
    assert!(
        !exec_marker.exists(),
        "the branch ran the script-level exec"
    );
    assert_eq!(report.served_calls, 0, "report: {report:?}");
    let divergence = report.divergence.expect("the refusal is the divergence");
    assert_eq!(divergence.what, "refused effect", "{divergence:?}");
    assert_eq!(divergence.call, 1, "the refusal came before model call 1");
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

/// A branch handed a state root that already holds state, here the
/// recording's own session store, is refused before anything runs.
#[test]
fn a_branch_refuses_a_state_root_that_already_holds_state() {
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
    })
    .err()
    .expect("a non-empty state root is refused");
    assert!(error.contains("is not empty"), "{error}");
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
