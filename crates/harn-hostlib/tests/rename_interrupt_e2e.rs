//! A person stopping one tool call stops a rename that waits on a language
//! server, through the real agent-loop dispatch path.
//!
//! The rename runs off the VM's thread, so the loop keeps running while the
//! server starts: progress reaches the session's event sink during the wait,
//! a cancel of that one call answers at once, and the rename writes nothing.
//! The server never reports quiescence, as rust-analyzer does while loading
//! a large workspace. It is selected through a process-wide environment
//! variable, which is why this file is its own test binary.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use harn_vm::agent_events::{AgentEvent, AgentEventSink};

const SESSION: &str = "rename-interrupt-session";
const CALL: &str = "rename_call_1";

const NEVER_QUIESCENT: &str = r#"#!/bin/sh
len=0
while IFS= read -r line; do
  line=$(printf '%s' "$line" | tr -d '\r')
  [ -z "$line" ] && break
  case "$line" in Content-Length:*) len=${line#Content-Length: } ;; esac
done
head -c "$len" > /dev/null
body='{"jsonrpc":"2.0","id":1,"result":{"capabilities":{"renameProvider":true}}}'
printf 'Content-Length: %s\r\n\r\n%s' "${#body}" "$body"
# fd 3 keeps the reply pipe open, so the client never reads an exit.
exec cat 3>&1 > /dev/null
"#;

const CRATE: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[package]\nname = \"small\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    ("src/lib.rs", "pub mod reading;\npub mod use_it;\n"),
    ("src/reading.rs", "pub struct Reading;\n"),
    (
        "src/use_it.rs",
        "use crate::reading::Reading;\n\npub fn keep(value: Reading) -> Reading {\n    value\n}\n",
    ),
];

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            // The code index keeps its snapshot in a hidden directory.
            if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'))
            {
                continue;
            }
            if path.is_dir() {
                pending.push(path);
            } else {
                files.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    files
}

/// Records progress phases and, on the rename's first heartbeat, cancels
/// that one tool call the way a host's stop control does.
#[derive(Default)]
struct StopOnHeartbeat {
    phases: Mutex<Vec<String>>,
    cancelled_at: Mutex<Option<Instant>>,
    stop_detail: Option<&'static str>,
    capture_handle: Option<Arc<Mutex<Option<harn_vm::tool_call_cancellations::Handle>>>>,
    /// When the loop reported the stopped call's terminal update.
    call_ended_at: Mutex<Option<Instant>>,
    /// Every terminal output of the call, so a failure says what it returned.
    call_outputs: Mutex<Vec<String>>,
}

impl AgentEventSink for StopOnHeartbeat {
    fn handle_event(&self, event: &AgentEvent) {
        if let AgentEvent::ToolCallUpdate {
            tool_call_id,
            error,
            raw_output: Some(raw_output),
            ..
        } = event
        {
            if tool_call_id == CALL {
                self.call_outputs
                    .lock()
                    .unwrap()
                    .push(format!("{raw_output:?} error={error:?}"));
            }
        }
        if let AgentEvent::ToolCallUpdate {
            tool_call_id,
            error: Some(_),
            ..
        } = event
        {
            if tool_call_id == CALL {
                self.call_ended_at
                    .lock()
                    .unwrap()
                    .get_or_insert_with(Instant::now);
            }
        }
        let AgentEvent::ToolCallUpdate {
            data: Some(data), ..
        } = event
        else {
            return;
        };
        let Some(progress) = data.get("progress") else {
            return;
        };
        self.phases
            .lock()
            .unwrap()
            .push(progress["phase"].as_str().unwrap_or_default().to_string());
        if progress["phase"].as_str() == Some("applying") {
            if let Some(capture) = &self.capture_handle {
                *capture.lock().unwrap() = harn_vm::tool_call_cancellations::lookup(SESSION, CALL);
            }
        }
        let heartbeat = self.stop_detail.map_or_else(
            || progress["elapsed_ms"].as_u64().unwrap_or_default() >= 1_000,
            |detail| progress["detail"].as_str() == Some(detail),
        );
        let mut cancelled_at = self.cancelled_at.lock().unwrap();
        if heartbeat && cancelled_at.is_none() {
            let outcome = harn_vm::tool_call_cancellations::cancel(
                SESSION,
                CALL,
                "person stopped the rename",
                false,
            );
            assert_eq!(outcome.status.as_str(), "cancelled");
            *cancelled_at = Some(Instant::now());
        }
    }
}

fn script(root: &str) -> String {
    let rename = |extra: &str| {
        format!(
            r#"harness.code_index.rename_symbol({{symbol_ref: {{name: "Reading", path: "src/reading.rs", kind: "Type"}}, new_name: "Measurement", scope: "workspace", {extra}}})"#
        )
    };
    // Both calls name the language server, so a server that cannot start
    // reports why instead of falling back and writing.
    let in_tool = rename(r#"backend: "language_server""#);
    let refused = rename(r#"backend: "language_server", deadline_ms: 300"#);
    format!(
        r#"import {{ agent_loop }} from "std/agent/loop"
import {{ agent_tool_handler_result }} from "std/agent/tool_lifecycle"
pipeline main(harness: Harness, _: unknown) {{
  harness.code_index.rebuild({{root: "{root}"}})
  harness.tools.clear_hooks()
  const tools = tool_define(tool_registry(), "rename_tool", "Rename Reading to Measurement.", {{
    parameters: {{}},
    handler: {{ _args ->
      const renamed = {in_tool}
      return agent_tool_handler_result(json_stringify(renamed), renamed)
    }},
  }})
  const counter = harness.runtime.shared_cell({{scope: "task_group", key: "rename-interrupt", initial: 0}})
  const mock_llm = {{ _call ->
    const snap = harness.runtime.shared_snapshot(counter)
    harness.runtime.shared_cas(counter, snap, snap.value + 1)
    if snap.value == 0 {{
      return {{ok: true, value: {{text: "", tool_calls: [{{id: "{CALL}", name: "rename_tool", arguments: {{}}}}], provider: "mock", model: "mock"}}}}
    }}
    return {{ok: true, value: {{text: "stopped ##DONE##", tool_calls: [], provider: "mock", model: "mock"}}}}
  }}
  const result = agent_loop(harness, "rename it", nil, {{
    provider: "mock", tools: tools, tool_format: "native", max_iterations: 4,
    loop_until_done: true, session_id: "{SESSION}", llm_caller: mock_llm,
  }})
  harness.stdio.log("status=" + result.status)
  let cancelled = false
  for msg in transcript_messages(result.transcript) {{
    if contains(to_string(msg?.content ?? ""), "cancelled call to rename_tool") {{
      cancelled = true
    }}
  }}
  harness.stdio.log("cancelled=" + to_string(cancelled))
  const refused = {refused}
  harness.stdio.log("refused=" + refused.result)
  harness.stdio.log("refused_details=" + refused.details)
  harness.stdio.log("refused_backend=" + to_string(refused.backend))
  harness.stdio.log("retry_after_ms=" + to_string(refused?.retry_after_ms))
  harness.stdio.log("alternative=" + to_string(refused?.alternative?.backend))
}}
"#
    )
}

#[test]
fn stopping_one_tool_call_stops_a_waiting_rename_without_writes() {
    let workspace = tempfile::tempdir().unwrap();
    for (path, text) in CRATE {
        let path = workspace.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    // A hidden directory keeps the stand-in server out of the snapshot.
    let program = workspace.path().join(".fake-server/rust-analyzer");
    fs::create_dir_all(program.parent().unwrap()).unwrap();
    fs::write(&program, NEVER_QUIESCENT).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: set before any thread of this test binary reads the variable.
    unsafe { std::env::set_var("HARN_LSP_SERVER_RUST_ANALYZER", &program) };
    // This test proves that a stop reaches the rename through the agent loop,
    // not how OS confinement treats a language server, so a host without a
    // process sandbox must still start the stand-in.
    // SAFETY: as above.
    unsafe { std::env::set_var("HARN_HANDLER_SANDBOX", "off") };
    // The agent loop confines tools to the working directory's workspace.
    std::env::set_current_dir(workspace.path()).unwrap();
    let before = snapshot(workspace.path());
    let root = workspace.path().to_string_lossy().replace('\\', "/");
    let chunk = harn_vm::compile_source(&script(&root)).expect("compile");
    let sink = Arc::new(StopOnHeartbeat::default());
    harn_vm::agent_events::register_sink(SESSION, sink.clone());

    let started = Instant::now();
    let output = harn_vm::on_vm_stack(|| {
        let rt = harn_clock::test_support::current_thread_runtime();
        rt.block_on(harn_clock::test_support::with_local_tasks(async {
            harn_vm::llm::install_current_host_bridge(Arc::new(
                harn_vm::bridge::HostBridge::from_parts(
                    Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    Arc::new(Mutex::new(())),
                    1,
                ),
            ));
            harn_vm::stdlib::process::set_session_environment(Some(
                harn_vm::security::SessionEnvironment::inherited(),
            ));
            // A host installs its policy before the loop runs. Without one, a
            // top-level loop requires OS confinement, which refuses to start
            // any program on a host without a process sandbox. This test is not
            // about confinement, so it asks for path enforcement only: a cold
            // macOS runner that cannot run sandbox-exec must still start the
            // stand-in server and reach the wait the test stops.
            harn_vm::orchestration::push_execution_policy(
                harn_vm::orchestration::CapabilityPolicy {
                    sandbox_profile: harn_vm::orchestration::SandboxProfile::WorkspacePaths,
                    ..Default::default()
                },
            );
            let mut vm = harn_vm::new_vm();
            harn_runtime::register_vm_stdlib(&mut vm);
            let _ = harn_hostlib::install_default(&mut vm);
            vm.set_harness(harn_vm::Harness::real());
            let result = vm.execute(&chunk).await;
            harn_vm::orchestration::pop_execution_policy();
            harn_vm::stdlib::process::set_session_environment(None);
            harn_vm::llm::clear_current_host_bridge();
            result.expect("script runs");
            vm.output().to_string()
        }))
    });
    let finished = Instant::now();
    harn_vm::agent_events::clear_session_sinks(SESSION);

    let lines: Vec<&str> = output
        .lines()
        .filter_map(|line| line.strip_prefix("[harn] "))
        .collect();
    eprintln!(
        "lines {lines:?}\nphases {:?}\ncall outputs {:?}",
        sink.phases.lock().unwrap(),
        sink.call_outputs.lock().unwrap()
    );
    let cancelled_at = sink.cancelled_at.lock().unwrap().unwrap_or_else(|| {
        panic!(
            "the rename never reported a heartbeat while it waited, so the stop was never \
             exercised; it returned {:?}",
            sink.call_outputs.lock().unwrap()
        )
    });
    let call_ended_at = sink
        .call_ended_at
        .lock()
        .unwrap()
        .expect("the stopped call reported its end");
    eprintln!(
        "total {:?}; cancel to call end {:?}; cancel to script end {:?}; lines {lines:?}",
        finished - started,
        call_ended_at - cancelled_at,
        finished - cancelled_at
    );
    assert_eq!(lines.first(), Some(&"status=done"), "{lines:?}");
    assert!(lines.contains(&"cancelled=true"), "{lines:?}");
    assert!(lines.contains(&"refused=refused"), "{lines:?}");
    assert!(lines.contains(&"alternative=syntax_tree"), "{lines:?}");
    let phases = sink.phases.lock().unwrap().clone();
    assert!(
        phases.iter().any(|phase| phase == "server_start"),
        "{phases:?}"
    );
    // The cold start budget is 30 s. The stopped call ends at once; the rest
    // of the loop and the refused call, in a debug build on a loaded host,
    // still finish well inside that budget.
    assert!(
        call_ended_at - cancelled_at < Duration::from_secs(2),
        "the stopped call ended {:?} after the cancel",
        call_ended_at - cancelled_at
    );
    assert!(
        finished - cancelled_at < Duration::from_secs(20),
        "the loop ran on {:?} after the cancel",
        finished - cancelled_at
    );
    assert_eq!(
        snapshot(workspace.path()),
        before,
        "a stopped rename must leave every file byte-identical"
    );
}

#[test]
fn stopping_a_go_workspace_census_leaves_the_package_unchanged() {
    stop_go_census("checking Go workspace sources", false, false, "workspace");
}

#[test]
fn stopping_a_go_package_census_leaves_the_package_unchanged() {
    stop_go_census("checking Go package sources", false, false, "workspace");
}

#[test]
fn stopping_a_go_scope_census_leaves_the_package_unchanged() {
    stop_go_census("checking Go scope bindings", false, false, "file");
}

#[test]
fn stopping_at_go_rename_commit_reports_the_write_and_stops_follow_on_work() {
    stop_go_census("hostlib_code_index_rename_symbol", true, false, "workspace");
}

#[test]
fn stopping_a_later_wait_after_go_rename_prevents_follow_on_work() {
    stop_go_census("never-match", true, true, "workspace");
}

fn stop_go_census(
    detail: &'static str,
    expect_applied: bool,
    stop_after_rename: bool,
    scope: &str,
) {
    let workspace = tempfile::tempdir().unwrap();
    fs::create_dir_all(workspace.path().join("src")).unwrap();
    fs::write(
        workspace.path().join("src/store.go"),
        "package lab\ntype Reading struct{}\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("src/use.go"),
        "package lab\nfunc Use(r *Reading) {}\n",
    )
    .unwrap();
    std::env::set_current_dir(workspace.path()).unwrap();
    let before = snapshot(workspace.path());
    let root = workspace.path().to_string_lossy().replace('\\', "/");
    let follow_on = format!("harness.fs.write_text(\"{root}/after-stop.txt\", \"unexpected\")");
    let later_wait = if stop_after_rename {
        "__test_wait_and_stop()"
    } else {
        ""
    };
    let captured_handle = Arc::new(Mutex::new(None::<harn_vm::tool_call_cancellations::Handle>));
    let source = format!(
        r#"import {{ agent_loop }} from "std/agent/loop"
import {{ agent_tool_handler_result }} from "std/agent/tool_lifecycle"
pipeline main(harness: Harness, _: unknown) {{
  harness.code_index.rebuild({{root: "{root}"}})
  harness.tools.clear_hooks()
  const tools = tool_define(tool_registry(), "rename_tool", "Rename Reading.", {{
    parameters: {{}},
    handler: {{ _args ->
      try {{
        const renamed = harness.code_index.rename_symbol({{symbol_ref: {{name: "Reading", path: "src/store.go", kind: "Type"}}, new_name: "Measurement", scope: "{scope}", backend: "syntax_tree"}})
        {later_wait}
        {follow_on}
        return agent_tool_handler_result(json_stringify(renamed), renamed)
      }} catch (error) {{
        {follow_on}
        throw error
      }}
    }},
  }})
  const counter = harness.runtime.shared_cell({{scope: "task_group", key: "go-rename-stop", initial: 0}})
  const mock_llm = {{ _call ->
    const snap = harness.runtime.shared_snapshot(counter)
    harness.runtime.shared_cas(counter, snap, snap.value + 1)
    if snap.value == 0 {{
      return {{ok: true, value: {{text: "", tool_calls: [{{id: "{CALL}", name: "rename_tool", arguments: {{}}}}], provider: "mock", model: "mock"}}}}
    }}
    return {{ok: true, value: {{text: "stopped ##DONE##", tool_calls: [], provider: "mock", model: "mock"}}}}
  }}
  const result = agent_loop(harness, "rename it", nil, {{
    provider: "mock", tools: tools, tool_format: "native", max_iterations: 4,
    loop_until_done: true, session_id: "{SESSION}", llm_caller: mock_llm,
  }})
  let cancelled = false
  let applied = false
  let changed_before_stop = false
  for msg in transcript_messages(result.transcript) {{
    if contains(to_string(msg?.content ?? ""), "cancelled call to rename_tool") {{ cancelled = true }}
    if contains(to_string(msg?.content ?? ""), "Files changed before stopping:") && contains(to_string(msg?.content ?? ""), "store.go") && contains(to_string(msg?.content ?? ""), "use.go") {{ changed_before_stop = true }}
    const data = msg?._harn?.data
    if data != nil && data?.result == "cancelled" && data?.applied == false && data?.touched_files != nil && len(data.touched_files) == 0 {{ cancelled = true }}
    if data != nil && data?.result == "applied" && data?.applied == true && data?.touched_files != nil && len(data.touched_files) == 2 {{ applied = true }}
  }}
  harness.stdio.log("cancelled=" + to_string(cancelled))
  harness.stdio.log("applied=" + to_string(applied))
  harness.stdio.log("changed_before_stop=" + to_string(changed_before_stop))
}}
"#
    );
    let chunk = harn_vm::compile_source(&source).expect("compile");
    let sink = Arc::new(StopOnHeartbeat {
        stop_detail: Some(detail),
        capture_handle: stop_after_rename.then_some(Arc::clone(&captured_handle)),
        ..Default::default()
    });
    harn_vm::agent_events::register_sink(SESSION, sink.clone());
    let output = harn_vm::on_vm_stack(|| {
        let rt = harn_clock::test_support::current_thread_runtime();
        rt.block_on(harn_clock::test_support::with_local_tasks(async {
            harn_vm::llm::install_current_host_bridge(Arc::new(
                harn_vm::bridge::HostBridge::from_parts(
                    Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    Arc::new(Mutex::new(())),
                    1,
                ),
            ));
            harn_vm::stdlib::process::set_session_environment(Some(
                harn_vm::security::SessionEnvironment::inherited(),
            ));
            harn_vm::orchestration::push_execution_policy(
                harn_vm::orchestration::CapabilityPolicy {
                    sandbox_profile: harn_vm::orchestration::SandboxProfile::WorkspacePaths,
                    ..Default::default()
                },
            );
            let mut vm = harn_vm::new_vm();
            harn_runtime::register_vm_stdlib(&mut vm);
            let _ = harn_hostlib::install_default(&mut vm);
            vm.set_harness(harn_vm::Harness::real());
            vm.register_async_builtin("__test_wait_and_stop", move |_ctx, _args| {
                let captured_handle = Arc::clone(&captured_handle);
                async move {
                    let handle = captured_handle
                        .lock()
                        .unwrap()
                        .clone()
                        .expect("live call handle");
                    assert!(handle.cancel("person stopped the later wait", false));
                    std::future::pending::<Result<harn_vm::VmValue, harn_vm::VmError>>().await
                }
            });
            let result =
                harn_clock::test_support::within("stopped rename handler", vm.execute(&chunk))
                    .await;
            harn_vm::orchestration::pop_execution_policy();
            harn_vm::stdlib::process::set_session_environment(None);
            harn_vm::llm::clear_current_host_bridge();
            result.expect("script runs");
            vm.output().to_string()
        }))
    });
    harn_vm::agent_events::clear_session_sinks(SESSION);
    eprintln!(
        "output {output}\nphases {:?}\ncall outputs {:?}",
        sink.phases.lock().unwrap(),
        sink.call_outputs.lock().unwrap()
    );
    if !stop_after_rename {
        assert!(
            sink.cancelled_at.lock().unwrap().is_some(),
            "Go census progress was never reached"
        );
    }
    assert!(
        !workspace.path().join("after-stop.txt").exists(),
        "the tool performed a second mutation after the stop"
    );
    if expect_applied {
        assert!(output.contains("cancelled=true"), "{output}");
        assert_eq!(
            fs::read_to_string(workspace.path().join("src/store.go")).unwrap(),
            "package lab\ntype Measurement struct{}\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.path().join("src/use.go")).unwrap(),
            "package lab\nfunc Use(r *Measurement) {}\n"
        );
        assert!(output.contains("changed_before_stop=true"), "{output}");
    } else {
        assert!(output.contains("cancelled=true"), "{output}");
        assert_eq!(
            snapshot(workspace.path()),
            before,
            "stopped rename changed files"
        );
    }
}
