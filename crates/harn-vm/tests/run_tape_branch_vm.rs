//! A branch replays a recorded run's prefix through the real agent loop
//! without re-running any of its effects: exec, runtime store writes and
//! tool hooks are served from the tape or refused, never run.
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

/// Each run keeps its own session store, as separate host runs do: the
/// branch's session id is served from the tape, so a shared session store
/// would hand it the recorded run's transcript.
fn run(source: &str, workspace: &Path, state: &str) -> Result<String, String> {
    let state_dir = workspace
        .parent()
        .expect("workspace has a parent")
        .join(state);
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
    let output = run(source, workspace, "record-state").expect("the recorded run succeeds");
    assert_eq!(guard.error(), None, "the tape wrote cleanly");
    drop(guard);
    output
}

fn branch(
    source: &str,
    tapes: &Path,
    workspace: &Path,
    k: u64,
) -> (Result<String, String>, BranchReport) {
    let guard = start_branch(BranchPlan {
        tape: EventTape::load(&tapes.join(TAPE_FILE_NAME)).unwrap(),
        live_after_calls: k,
        workspace_root: workspace.to_path_buf(),
    })
    .unwrap();
    let output = run(source, workspace, "branch-state");
    let report = guard.finish();
    (output, report)
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

    let (output, report) = branch(&source, &dirs.tapes, &dirs.workspace, 2);
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
        "the branch wrote the store file"
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
    std::fs::remove_file(harn_vm::runtime_paths::store_path(&dirs.workspace)).unwrap();

    let (output, report) = branch(&source, &dirs.tapes, &dirs.workspace, 1);
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
    let store_path = harn_vm::runtime_paths::store_path(&dirs.workspace);
    let store: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&store_path).unwrap()).unwrap();
    assert_eq!(
        store["after_key"], "done",
        "a live-tail write saves: {store}"
    );
    assert!(
        store["script_key"].is_string(),
        "prefix writes carry over: {store}"
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

    let (output, report) = branch(&source, &dirs.tapes, &dirs.workspace, 1);
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

/// One `mark` tool call, then done, with a post-turn session reset: the reset
/// runs inside the live run, so its durable write is the run journal's.
fn reset_script(hook_marker: &Path, exec_marker: &Path) -> String {
    mark_script(hook_marker, exec_marker).replace(
        "          session_id: \"branch-\" + id,",
        "          session_id: \"branch-\" + id,\n          post_turn_callback: { _turn -> \n            harness.agent.reset(\"branch-\" + id)\n            return nil\n          },",
    )
}

/// Every event the durable session store under `state` holds, as `kind`
/// joined with the payload's text, across every session in it.
fn durable_events(workspace: &Path, state: &str) -> Vec<String> {
    let root = workspace.parent().unwrap().join(state);
    let mut found = Vec::new();
    let mut dirs = vec![root];
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
                for event in store.read_all(&meta.id).await.unwrap() {
                    events.push(format!("{:?} {}", event.kind, event.payload));
                }
            }
        }
        events
    })
}

/// Session resets the durable store under `state` holds: a reset lands as an
/// empty compaction.
fn durable_resets(workspace: &Path, state: &str) -> usize {
    durable_events(workspace, state)
        .iter()
        .filter(|event| event.starts_with("Compaction ") && event.contains("\"messages\":[]"))
        .count()
}

/// A reset inside the replayed prefix changes the session in memory but
/// writes nothing durable; once the branch goes live, the queued journal
/// writes it, so the branch's history matches a real run's.
#[test]
fn a_session_reset_in_the_prefix_is_written_once_and_only_after_going_live() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let source = reset_script(&hook_marker, &exec_marker);
    record(&source, &dirs.tapes, &dirs.workspace);
    assert_eq!(
        durable_resets(&dirs.workspace, "record-state"),
        2,
        "one reset per turn"
    );

    let (output, report) = branch(&source, &dirs.tapes, &dirs.workspace, 2);
    output.expect("the replayed run succeeds");
    assert_eq!(report.divergence, None, "report: {report:?}");
    assert_eq!(
        durable_resets(&dirs.workspace, "branch-state"),
        0,
        "a fully replayed branch wrote a reset durably"
    );

    std::fs::remove_dir_all(dirs.workspace.parent().unwrap().join("branch-state")).unwrap();
    let (output, report) = branch(&source, &dirs.tapes, &dirs.workspace, 1);
    output.expect("the branched run succeeds");
    assert_eq!(report.divergence, None, "report: {report:?}");
    assert_eq!(report.live_calls, 1, "report: {report:?}");
    assert_eq!(
        durable_resets(&dirs.workspace, "branch-state"),
        2,
        "the prefix reset reaches the branch's history once, after going live"
    );
}

/// Prefix store writes reach disk when the branch goes live, even when the
/// live tail never writes the store itself.
#[test]
fn prefix_store_writes_are_saved_when_the_branch_goes_live() {
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
    let store_path = harn_vm::runtime_paths::store_path(&dirs.workspace);
    let sentinel = "{\"sentinel\": true}";
    std::fs::write(&store_path, sentinel).unwrap();

    let (output, report) = branch(&source, &dirs.tapes, &dirs.workspace, 1);
    output.expect("the branched run succeeds");
    assert_eq!(report.live_calls, 1, "report: {report:?}");
    let store: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&store_path).unwrap()).unwrap();
    assert!(
        store["script_key"].is_string(),
        "the prefix write was saved: {store}"
    );
    assert!(
        store.get("sentinel").is_none(),
        "the store started from the seed: {store}"
    );
}
