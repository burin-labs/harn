use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use harn_vm::{Vm, VmValue};
use serde_json::json;
use tempfile::TempDir;

use super::state::{Debugger, StepMode};
use crate::protocol::{DapMessage, DapResponse};

fn make_request(seq: i64, command: &str, args: Option<serde_json::Value>) -> DapMessage {
    DapMessage {
        seq,
        msg_type: "request".to_string(),
        command: Some(command.to_string()),
        arguments: args,
        request_seq: None,
        success: None,
        message: None,
        body: None,
    }
}

fn write_temp_program(file_name: &str, source: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp debugger workspace");
    let file = dir.path().join(file_name);
    std::fs::write(&file, source).expect("write temp Harn program");
    (dir, file)
}

fn file_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .expect("absolute temporary path should convert to a file URI")
        .to_string()
}

fn localhost_file_uri(path: &Path) -> String {
    let uri = file_uri(path);
    format!(
        "file://localhost{}",
        uri.strip_prefix("file://")
            .expect("file_uri should produce a file scheme URI")
    )
}

fn run_debug_program(file_name: &str, source: &str) -> Vec<DapResponse> {
    let mut dbg = Debugger::new();
    let (_dir, file) = write_temp_program(file_name, source);

    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));

    let mut responses = dbg.handle_message(make_request(3, "configurationDone", None));
    let mut steps = 0usize;
    while dbg.is_running() {
        responses.extend(dbg.step_running_vm());
        steps += 1;
        assert!(steps < 20_000, "debug program did not terminate");
    }
    responses
}

fn stdout_text(responses: &[DapResponse]) -> String {
    responses
        .iter()
        .filter(|r| {
            r.event.as_deref() == Some("output")
                && r.body
                    .as_ref()
                    .map(|b| b["category"] == "stdout")
                    .unwrap_or(false)
        })
        .filter_map(|r| {
            r.body
                .as_ref()
                .and_then(|body| body["output"].as_str())
                .map(str::to_string)
        })
        .collect::<String>()
}

#[test]
fn flush_output_recovers_when_cached_output_is_not_a_prefix() {
    let mut dbg = Debugger::new();
    let mut vm = Vm::new();
    vm.append_output("new\n");
    dbg.vm = Some(vm);
    dbg.output = "stale and longer".to_string();

    let mut responses = Vec::new();
    dbg.flush_output_into(&mut responses);

    assert_eq!(stdout_text(&responses), "new\n");
    assert_eq!(dbg.output, "new\n");
}

#[test]
fn test_initialize() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(1, "initialize", None));
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0].command.as_deref(), Some("initialize"));
    assert_eq!(responses[0].success, Some(true));
    assert_eq!(responses[1].event.as_deref(), Some("initialized"));
}

#[test]
fn test_threads() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(1, "threads", None));
    assert_eq!(responses.len(), 1);
    let body = responses[0].body.as_ref().unwrap();
    let threads = body["threads"].as_array().unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0]["id"], 1);
    assert_eq!(threads[0]["name"], "main");
}

#[test]
fn test_two_sessions_get_distinct_thread_ids() {
    let mut dbg = Debugger::new();
    let a = dbg.register_thread("session-A");
    let b = dbg.register_thread("session-B");
    assert_ne!(a, b, "distinct sessions must map to distinct thread ids");
    assert!(
        a >= 2 && b >= 2,
        "allocated ids must not collide with main=1"
    );

    let responses = dbg.handle_message(make_request(1, "threads", None));
    let body = responses[0].body.as_ref().unwrap();
    let threads = body["threads"].as_array().unwrap();
    assert_eq!(threads.len(), 3, "main + two sessions");

    // Re-registering the same session is idempotent.
    let a2 = dbg.register_thread("session-A");
    assert_eq!(a, a2);
}

#[test]
fn test_register_thread_emits_started_event() {
    let mut dbg = Debugger::new();
    let id = dbg.register_thread("session-X");
    let evt = dbg.thread_started_event(id);
    assert_eq!(evt.event.as_deref(), Some("thread"));
    let body = evt.body.as_ref().unwrap();
    assert_eq!(body["reason"], "started");
    assert_eq!(body["threadId"], id as i64);
}

#[test]
fn test_unregister_thread_emits_exited_event_but_main_survives() {
    let mut dbg = Debugger::new();
    let id = dbg.register_thread("session-Y");
    let freed = dbg.unregister_thread("session-Y").expect("must free");
    assert_eq!(freed, id);

    let evt = dbg.thread_exited_event(freed);
    assert_eq!(evt.event.as_deref(), Some("thread"));
    assert_eq!(evt.body.as_ref().unwrap()["reason"], "exited");

    // Unregistering a nonexistent session is a no-op.
    assert!(dbg.unregister_thread("never-registered").is_none());

    // Main thread must never be removable even if aliased.
    dbg.session_to_thread.insert("main".to_string(), 1);
    assert!(dbg.unregister_thread("main").is_none());
    assert!(dbg.threads.contains_key(&1));
}

#[test]
fn test_stepping_events_carry_current_thread_id() {
    // MVP: current_thread_id defaults to 1 and the stepping handlers
    // read from it. With only one VM we can't actually run a second
    // session to completion, but we can verify that rewriting the
    // field routes subsequent events to the new id.
    let mut dbg = Debugger::new();
    let new_id = dbg.register_thread("session-thread-check");
    dbg.current_thread_id = new_id;

    // A pause on an already-stopped debugger emits a stopped event
    // with reason="pause" and the active threadId.
    dbg.stopped = true;
    let responses = dbg.handle_message(make_request(1, "pause", None));
    let stopped = responses
        .iter()
        .find(|r| r.event.as_deref() == Some("stopped"))
        .expect("pause must emit stopped");
    let body = stopped.body.as_ref().unwrap();
    assert_eq!(body["threadId"], new_id as i64);
    assert_eq!(body["reason"], "pause");
}

#[test]
fn test_set_breakpoints() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(
        1,
        "setBreakpoints",
        Some(json!({
            "source": {"path": "test.harn"},
            "breakpoints": [{"line": 5}, {"line": 10}]
        })),
    ));
    assert_eq!(responses.len(), 1);
    let body = responses[0].body.as_ref().unwrap();
    let bps = body["breakpoints"].as_array().unwrap();
    assert_eq!(bps.len(), 2);
    assert_eq!(bps[0]["line"], 5);
    assert_eq!(bps[1]["line"], 10);
    assert_eq!(bps[0]["verified"], true);
}

#[test]
fn test_launch_and_run() {
    let mut dbg = Debugger::new();

    let (_dir, file) = write_temp_program("test.harn", "pipeline test(task) { log(42) }");

    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));

    // configurationDone transitions the debugger into "running" and
    // returns immediately; the main loop drives step_running_vm
    // between message drains. In tests we drain manually until the
    // program terminates.
    let mut responses = dbg.handle_message(make_request(3, "configurationDone", None));
    while dbg.is_running() {
        responses.extend(dbg.step_running_vm());
    }
    assert!(responses.len() >= 2);

    let output_event = responses.iter().find(|r| {
        r.event.as_deref() == Some("output")
            && r.body
                .as_ref()
                .map(|b| b["category"] == "stdout")
                .unwrap_or(false)
    });

    if let Some(evt) = output_event {
        let output = evt.body.as_ref().unwrap()["output"].as_str().unwrap();
        assert!(output.contains("[harn] 42"));
    }

    let terminated = responses
        .iter()
        .find(|r| r.event.as_deref() == Some("terminated"));
    assert!(terminated.is_some());
    drop(dbg);
}

#[test]
fn test_parallel_runs_under_debugger_runtime() {
    let responses = run_debug_program(
        "parallel_debugger_runtime.harn",
        r"
pipeline test(task) {
  let results = parallel 3 { i -> i * 10 }
  log(results)
}
",
    );
    assert!(
        responses
            .iter()
            .any(|r| r.event.as_deref() == Some("terminated")),
        "parallel debug run should terminate: {responses:?}"
    );
    let output = stdout_text(&responses);
    assert!(
        output.contains("[harn] [0, 10, 20]"),
        "parallel output missing from debugger stdout: {output:?}"
    );
}

#[test]
fn test_pool_workers_run_under_debugger_runtime() {
    let responses = run_debug_program(
        "pool_debugger_runtime.harn",
        r#"
import { pool_create, pool_wait } from "std/lifecycle/pool"

pipeline test(task) {
  let pool = pool_create({name: "dap-pool-runtime", max_concurrent: 4})
  var handles = []
  for i in 0 to 8 exclusive {
    let seed = i
    handles = handles.push(pool.submit({ -> seed + 10 }))
  }
  let results = pool_wait(handles)
  var completed = 0
  for result in results {
    if result.status == "completed" && result.result >= 10 {
      completed = completed + 1
    }
  }
  log(completed)
}
"#,
    );
    assert!(
        responses
            .iter()
            .any(|r| r.event.as_deref() == Some("terminated")),
        "pool debug run should terminate: {responses:?}"
    );
    let output = stdout_text(&responses);
    assert!(
        output.contains("[harn] 8"),
        "pool output missing from debugger stdout: {output:?}"
    );
}

#[test]
fn test_scopes_and_variables() {
    let mut dbg = Debugger::new();
    dbg.variables.insert("x".to_string(), VmValue::Int(42));
    dbg.variables
        .insert("name".to_string(), VmValue::String("hello".into()));

    let responses = dbg.handle_message(make_request(
        1,
        "variables",
        Some(json!({"variablesReference": 1})),
    ));
    assert_eq!(responses.len(), 1);
    let body = responses[0].body.as_ref().unwrap();
    let vars = body["variables"].as_array().unwrap();
    assert_eq!(vars.len(), 2);
}

#[test]
fn test_evaluate() {
    let mut dbg = Debugger::new();
    dbg.variables.insert("x".to_string(), VmValue::Int(42));

    let responses = dbg.handle_message(make_request(
        1,
        "evaluate",
        Some(json!({"expression": "x"})),
    ));
    assert_eq!(responses.len(), 1);
    let body = responses[0].body.as_ref().unwrap();
    assert_eq!(body["result"], "42");
    assert_eq!(body["variablesReference"], 0);
}

#[test]
fn test_evaluate_dot_access() {
    use std::sync::Arc;

    let mut dbg = Debugger::new();
    let mut inner = BTreeMap::new();
    inner.insert("bar".to_string(), VmValue::Int(99));
    dbg.variables
        .insert("foo".to_string(), VmValue::Dict(Arc::new(inner)));

    let responses = dbg.handle_message(make_request(
        1,
        "evaluate",
        Some(json!({"expression": "foo.bar"})),
    ));
    assert_eq!(responses.len(), 1);
    let body = responses[0].body.as_ref().unwrap();
    assert_eq!(body["result"], "99");
    assert_eq!(body["variablesReference"], 0);
}

#[test]
fn test_evaluate_nested_dot_access() {
    use std::sync::Arc;

    let mut dbg = Debugger::new();
    let mut inner = BTreeMap::new();
    inner.insert("c".to_string(), VmValue::String("deep".into()));
    let mut outer = BTreeMap::new();
    outer.insert("b".to_string(), VmValue::Dict(Arc::new(inner)));
    dbg.variables
        .insert("a".to_string(), VmValue::Dict(Arc::new(outer)));

    let responses = dbg.handle_message(make_request(
        1,
        "evaluate",
        Some(json!({"expression": "a.b.c"})),
    ));
    assert_eq!(responses.len(), 1);
    let body = responses[0].body.as_ref().unwrap();
    assert_eq!(body["result"], "deep");
}

#[test]
fn test_evaluate_complex_value_has_var_ref() {
    use std::sync::Arc;

    let mut dbg = Debugger::new();
    let mut map = BTreeMap::new();
    map.insert("key".to_string(), VmValue::Int(1));
    dbg.variables
        .insert("d".to_string(), VmValue::Dict(Arc::new(map)));

    let responses = dbg.handle_message(make_request(
        1,
        "evaluate",
        Some(json!({"expression": "d"})),
    ));
    assert_eq!(responses.len(), 1);
    let body = responses[0].body.as_ref().unwrap();
    assert!(body["variablesReference"].as_i64().unwrap() > 0);
}

#[test]
fn test_evaluate_undefined_returns_error() {
    let mut dbg = Debugger::new();

    let responses = dbg.handle_message(make_request(
        1,
        "evaluate",
        Some(json!({"expression": "nonexistent"})),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(false));
    assert!(responses[0]
        .message
        .as_ref()
        .unwrap()
        .contains("nonexistent"));
}

#[test]
fn test_evaluate_with_context() {
    let mut dbg = Debugger::new();
    dbg.variables.insert("x".to_string(), VmValue::Int(7));

    for ctx in &["watch", "repl", "hover"] {
        let responses = dbg.handle_message(make_request(
            1,
            "evaluate",
            Some(json!({"expression": "x", "context": ctx})),
        ));
        assert_eq!(responses.len(), 1);
        let body = responses[0].body.as_ref().unwrap();
        assert_eq!(body["result"], "7");
    }
}

#[test]
fn test_source_returns_launch_source_content() {
    let mut dbg = Debugger::new();
    dbg.source_path = Some("/tmp/main.harn".to_string());
    dbg.source_content = Some("let x = 1\n".to_string());

    let responses = dbg.handle_message(make_request(
        1,
        "source",
        Some(json!({"source": {"path": "/tmp/main.harn"}})),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(true));
    let body = responses[0].body.as_ref().unwrap();
    assert_eq!(body["content"], "let x = 1\n");
    assert_eq!(body["mimeType"], "text/x-harn");
}

#[test]
fn test_source_reads_prompt_template_from_disk() {
    let (_dir, file) = write_temp_program("sample.harn.prompt", "Hello {{ name }}\n");
    let responses = Debugger::new().handle_message(make_request(
        1,
        "source",
        Some(json!({"source": {"path": file.to_string_lossy()}})),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(true));
    let body = responses[0].body.as_ref().unwrap();
    assert_eq!(body["content"], "Hello {{ name }}\n");
    assert_eq!(body["mimeType"], "text/x-harn-prompt");
}

#[test]
fn test_source_decodes_file_uri_paths() {
    let (dir, file) = write_temp_program("space name.harn", "pipeline test(task) {}\n");

    for (seq, uri) in [file_uri(&file), localhost_file_uri(&file)]
        .into_iter()
        .enumerate()
    {
        let responses = Debugger::new().handle_message(make_request(
            seq as i64 + 1,
            "source",
            Some(json!({"source": {"path": uri}})),
        ));
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].success, Some(true), "{responses:?}");
        let body = responses[0].body.as_ref().unwrap();
        assert_eq!(body["content"], "pipeline test(task) {}\n");
    }
    drop(dir);
}

#[test]
fn test_source_reads_stdlib_synthetic_source() {
    let responses = Debugger::new().handle_message(make_request(
        1,
        "source",
        Some(json!({"source": {"path": "<stdlib>/text.harn"}})),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(true));
    let body = responses[0].body.as_ref().unwrap();
    assert!(body["content"].as_str().unwrap().contains("pub fn"));
}

#[test]
fn test_source_reference_reads_vm_generated_source() {
    let source = "pub fn generated_answer() {\n  return 42\n}\n";
    let mut vm = Vm::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    runtime
        .block_on(vm.load_module_exports_from_source("<generated>/wrapper.harn", source))
        .expect("load generated module");

    let mut dbg = Debugger::new();
    dbg.vm = Some(vm);
    let source_ref = dbg
        .source_for_path("<generated>/wrapper.harn")
        .source_reference
        .expect("generated source gets a sourceReference");

    let responses = dbg.handle_message(make_request(
        1,
        "source",
        Some(json!({"sourceReference": source_ref})),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(true));
    let body = responses[0].body.as_ref().unwrap();
    assert_eq!(body["content"], source);
}

#[test]
fn test_set_exception_breakpoints_enable() {
    let mut dbg = Debugger::new();
    assert!(!dbg.break_on_exceptions);

    let responses = dbg.handle_message(make_request(
        1,
        "setExceptionBreakpoints",
        Some(json!({"filters": ["all"]})),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(true));
    assert!(dbg.break_on_exceptions);
}

#[test]
fn test_set_exception_breakpoints_disable() {
    let mut dbg = Debugger::new();
    dbg.break_on_exceptions = true;

    let responses = dbg.handle_message(make_request(
        1,
        "setExceptionBreakpoints",
        Some(json!({"filters": []})),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(true));
    assert!(!dbg.break_on_exceptions);
}

#[test]
fn test_initialize_has_exception_breakpoint_filters() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(1, "initialize", None));
    let body = responses[0].body.as_ref().unwrap();
    assert_eq!(body["supportsExceptionBreakpointFilters"], true);
    let filters = body["exceptionBreakpointFilters"].as_array().unwrap();
    // #111 expanded the list to "all" plus four per-kind filters;
    // #1868 (DAP subagents) appended three more lifecycle filters.
    assert_eq!(filters.len(), 8);
    let filter_ids: Vec<_> = filters
        .iter()
        .filter_map(|f| f["filter"].as_str())
        .collect();
    assert!(filter_ids.contains(&"all"));
    assert!(filter_ids.contains(&"tool_error"));
    assert!(filter_ids.contains(&"llm_refusal"));
    assert!(filter_ids.contains(&"budget_exceeded"));
    assert!(filter_ids.contains(&"parse_failure"));
    assert!(filter_ids.contains(&"break-on-suspend"));
    assert!(filter_ids.contains(&"break-on-resume"));
    assert!(filter_ids.contains(&"break-on-drain-decision"));
    let all_filter = filters
        .iter()
        .find(|f| f["filter"].as_str() == Some("all"))
        .unwrap();
    assert_eq!(all_filter["label"], "All Exceptions");
    assert_eq!(all_filter["default"], false);
}

#[test]
fn test_step_commands() {
    let mut dbg = Debugger::new();

    let r = dbg.handle_message(make_request(1, "next", None));
    assert!(r[0].success == Some(true));
    assert_eq!(dbg.step_mode, StepMode::StepOver);

    let r = dbg.handle_message(make_request(2, "stepIn", None));
    assert!(r[0].success == Some(true));
    assert_eq!(dbg.step_mode, StepMode::StepIn);

    let r = dbg.handle_message(make_request(3, "stepOut", None));
    assert!(r[0].success == Some(true));
    assert_eq!(dbg.step_mode, StepMode::StepOut);

    let r = dbg.handle_message(make_request(4, "continue", None));
    assert!(r[0].success == Some(true));
    assert_eq!(dbg.step_mode, StepMode::Continue);
}

#[test]
fn test_disconnect() {
    let mut dbg = Debugger::new();
    let r = dbg.handle_message(make_request(1, "disconnect", None));
    assert_eq!(r[0].success, Some(true));
}

#[test]
fn test_stack_trace() {
    let mut dbg = Debugger::new();
    dbg.source_path = Some("test.harn".to_string());
    dbg.current_line = 5;

    let r = dbg.handle_message(make_request(1, "stackTrace", None));
    let body = r[0].body.as_ref().unwrap();
    let frames = body["stackFrames"].as_array().unwrap();
    assert_eq!(frames.len(), 1);
}

#[test]
fn test_breakpoint_stop() {
    let mut dbg = Debugger::new();

    let (_dir, file) = write_temp_program(
        "test_bp.harn",
        "pipeline test(task) {\n  let x = 1\n  let y = 2\n  log(x + y)\n}",
    );

    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "setBreakpoints",
        Some(json!({
            "source": {"path": file.to_string_lossy()},
            "breakpoints": [{"line": 3}]
        })),
    ));
    dbg.handle_message(make_request(
        3,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));

    let mut responses = dbg.handle_message(make_request(4, "configurationDone", None));
    while dbg.is_running() {
        responses.extend(dbg.step_running_vm());
    }

    // A path-keyed breakpoint on the entry script MUST halt execution with
    // reason="breakpoint". Prior to the source_file fix, the main chunk was
    // untagged so `Vm::breakpoint_matches` could never match the absolute
    // path the client sent, and the program raced to terminated -- this
    // assertion pins that regression.
    let stopped_on_breakpoint = responses.iter().any(|r| {
        r.event.as_deref() == Some("stopped")
            && r.body
                .as_ref()
                .and_then(|b| b.get("reason"))
                .and_then(|v| v.as_str())
                == Some("breakpoint")
    });
    assert!(
        stopped_on_breakpoint,
        "expected a stopped event with reason=breakpoint for the entry script"
    );
    drop(dbg);
}

#[test]
fn test_clearing_breakpoints_updates_live_vm() {
    let mut dbg = Debugger::new();

    let (_dir, file) = write_temp_program(
        "clear_breakpoints.harn",
        "pipeline test(task) {\n  let x = 1\n  let y = 2\n  log(x + y)\n}\n",
    );
    let path = file.to_string_lossy().to_string();

    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "setBreakpoints",
        Some(json!({
            "source": {"path": path},
            "breakpoints": [{"line": 3}]
        })),
    ));
    dbg.handle_message(make_request(
        3,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));
    dbg.handle_message(make_request(
        4,
        "setBreakpoints",
        Some(json!({
            "source": {"path": file.to_string_lossy()},
            "breakpoints": []
        })),
    ));

    let mut responses = dbg.handle_message(make_request(5, "configurationDone", None));
    while dbg.is_running() {
        responses.extend(dbg.step_running_vm());
    }

    assert!(
        responses
            .iter()
            .all(|r| r.event.as_deref() != Some("stopped")),
        "cleared breakpoint should not stop the VM: {responses:?}"
    );
    assert!(
        responses
            .iter()
            .any(|r| r.event.as_deref() == Some("terminated")),
        "program should run through after breakpoint clear"
    );
    drop(dbg);
}

#[test]
fn test_pause_interrupts_running_vm() {
    let mut dbg = Debugger::new();

    let (_dir, file) = write_temp_program(
        "test_pause.harn",
        "pipeline test(task) {\n  let x = 1\n  let y = 2\n  let z = 3\n}",
    );

    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));
    dbg.handle_message(make_request(3, "configurationDone", None));
    assert!(dbg.is_running());

    // Pause the in-flight VM before draining; the next step tick
    // must honor the pending pause and emit stopped/reason=pause.
    dbg.handle_message(make_request(4, "pause", None));
    let step_responses = dbg.step_running_vm();
    let paused = step_responses.iter().any(|r| {
        r.event.as_deref() == Some("stopped")
            && r.body
                .as_ref()
                .map(|b| b["reason"] == "pause")
                .unwrap_or(false)
    });
    assert!(paused, "expected a stopped event with reason=pause");
    assert!(!dbg.is_running());
    drop(dbg);
}

#[test]
fn test_harn_ping_reports_state() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(1, "harnPing", None));
    assert_eq!(responses.len(), 1);
    let body = responses[0].body.as_ref().unwrap();
    assert_eq!(body["state"], "not_started");
    assert_eq!(body["running"], false);
    assert_eq!(body["stopped"], false);
}

#[test]
fn test_completions_returns_frame_scope_and_builtins() {
    let mut dbg = Debugger::new();

    let (_dir, file) = write_temp_program(
        "completions.harn",
        "pipeline t(task) { let local_name = 1\nlog(local_name) }",
    );
    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));

    let responses = dbg.handle_message(make_request(
        3,
        "completions",
        Some(json!({ "text": "", "frameId": 1 })),
    ));
    let body = responses[0].body.as_ref().unwrap();
    let targets = body["targets"].as_array().unwrap();
    assert!(
        !targets.is_empty(),
        "completions response must surface at least the builtin namespace"
    );
    // The built-in `log` is always registered; assert it's present
    // regardless of user code.
    let labels: Vec<_> = targets.iter().filter_map(|t| t["label"].as_str()).collect();
    assert!(labels.contains(&"log"), "builtin log must appear");
    drop(dbg);
}

#[test]
fn test_step_in_targets_returns_targets_shape_even_without_match() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(
        1,
        "stepInTargets",
        Some(json!({ "frameId": 1 })),
    ));
    assert_eq!(responses.len(), 1);
    let body = responses[0].body.as_ref().unwrap();
    let targets = body["targets"].as_array().unwrap();
    // Without a live VM we can't derive call sites, but the response
    // shape must still be well-formed with an empty list.
    assert!(targets.is_empty());
}

#[test]
fn test_exception_filters_track_per_kind_selection() {
    let mut dbg = Debugger::new();
    dbg.handle_message(make_request(
        1,
        "setExceptionBreakpoints",
        Some(json!({
            "filters": ["tool_error"],
            "filterOptions": [
                { "filterId": "llm_refusal", "condition": "kind == \"x\"" }
            ]
        })),
    ));
    assert_eq!(dbg.exception_filters.len(), 2);
    assert!(dbg.exception_filters.contains_key("tool_error"));
    assert_eq!(
        dbg.exception_filters
            .get("llm_refusal")
            .cloned()
            .flatten()
            .as_deref(),
        Some("kind == \"x\"")
    );
    // Replacing the set clears stale entries.
    dbg.handle_message(make_request(
        2,
        "setExceptionBreakpoints",
        Some(json!({ "filters": ["parse_failure"] })),
    ));
    assert_eq!(dbg.exception_filters.len(), 1);
    assert!(dbg.exception_filters.contains_key("parse_failure"));
}

#[test]
fn test_invalidated_event_carries_areas_and_thread_id() {
    let mut dbg = Debugger::new();
    let evt = dbg.invalidated_event(vec!["variables", "threads"]);
    assert_eq!(evt.event.as_deref(), Some("invalidated"));
    let body = evt.body.as_ref().unwrap();
    let areas = body["areas"].as_array().unwrap();
    assert_eq!(areas.len(), 2);
    assert_eq!(body["threadId"], 1); // current_thread_id default
}

#[test]
fn test_cancel_handler_responds_success_even_without_match() {
    let mut dbg = Debugger::new();
    // No host bridge, no pending requests — cancel still succeeds so
    // the IDE's Stop pill never flashes a red error on a
    // tear-down race.
    let responses = dbg.handle_message(make_request(1, "cancel", Some(json!({ "requestId": 42 }))));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(true));
    assert_eq!(responses[0].command.as_deref(), Some("cancel"));
}

#[test]
fn test_capabilities_advertise_new_bp_features() {
    // Capabilities snapshot so UI writers know what's actually wired
    // — bumping a capability without flipping the default is a silent
    // regression that breaks the IDE's breakpoint popover.
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(1, "initialize", None));
    let body = responses[0].body.as_ref().unwrap();
    assert_eq!(body["supportsConditionalBreakpoints"], true);
    assert_eq!(body["supportsHitConditionalBreakpoints"], true);
    assert_eq!(body["supportsLogPoints"], true);
    assert_eq!(body["supportsFunctionBreakpoints"], true);
    assert_eq!(body["supportsSetVariable"], true);
    assert_eq!(body["supportsSetExpression"], true);
    assert_eq!(body["supportsRestartFrame"], true);
    assert_eq!(body["supportsBurinPromptProvenance"], true);
    assert_eq!(body["supportsModulesRequest"], true);
    assert_eq!(body["supportsLoadedSourcesRequest"], true);
    assert_eq!(body["supportsTerminateRequest"], true);
}

#[test]
fn test_terminate_emits_terminated_event() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(1, "terminate", None));
    // Response + terminated event.
    assert!(responses
        .iter()
        .any(|r| r.command.as_deref() == Some("terminate") && r.success == Some(true)));
    assert!(responses
        .iter()
        .any(|r| r.event.as_deref() == Some("terminated")));
    assert!(!dbg.is_running());
}

#[test]
fn test_loaded_sources_reports_entry_program() {
    let mut dbg = Debugger::new();
    let (_dir, file) = write_temp_program("loaded.harn", "pipeline t(task) { log(\"hi\") }");
    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));
    let responses = dbg.handle_message(make_request(3, "loadedSources", None));
    let body = responses[0].body.as_ref().unwrap();
    let sources = body["sources"].as_array().unwrap();
    assert!(
        sources
            .iter()
            .any(|s| s.get("path").and_then(|p| p.as_str()).is_some()),
        "loadedSources must include at least the entry script's path"
    );
}

#[test]
fn test_modules_reports_entry_module() {
    let mut dbg = Debugger::new();
    let (_dir, file) = write_temp_program("mods.harn", "pipeline t(task) { log(\"hi\") }");
    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));
    let responses = dbg.handle_message(make_request(3, "modules", None));
    let body = responses[0].body.as_ref().unwrap();
    let modules = body["modules"].as_array().unwrap();
    assert!(!modules.is_empty(), "modules list must not be empty");
    let total = body["totalModules"].as_u64().unwrap_or(0);
    assert!(total >= 1);
    // Module entries must carry id + name + path.
    let first = &modules[0];
    assert!(first.get("id").is_some());
    assert!(first.get("name").is_some());
    assert!(first.get("path").is_some());
}

#[test]
fn test_modules_with_explicit_zero_module_count_returns_all() {
    // Per DAP, `moduleCount: 0` is paging-disabled and means "all
    // remaining". The previous `.max(1)` silently returned a single
    // entry — this regression test pins the new behavior.
    let mut dbg = Debugger::new();
    let (_dir, file) = write_temp_program("zerocount.harn", "pipeline t(task) { log(\"hi\") }");
    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));
    let responses = dbg.handle_message(make_request(
        3,
        "modules",
        Some(json!({"startModule": 0, "moduleCount": 0})),
    ));
    let body = responses[0].body.as_ref().unwrap();
    let modules = body["modules"].as_array().unwrap();
    let total = body["totalModules"].as_u64().unwrap_or(0);
    assert_eq!(
        modules.len() as u64,
        total,
        "moduleCount: 0 must return every module (paging disabled)"
    );
}

#[test]
fn test_triggered_breakpoint_skipped_until_trigger_fires() {
    use super::breakpoints::BreakpointAction;
    use std::collections::BTreeMap;

    let mut dbg = Debugger::new();
    // Arrange two breakpoints: trigger (id=1, line 5) and dependent
    // (id=2, line 10, triggered_by: [1]).
    dbg.handle_message(make_request(
        1,
        "setBreakpoints",
        Some(json!({
            "source": {"path": "t.harn"},
            "breakpoints": [
                {"line": 5},
                {"line": 10, "triggeredBy": [1]}
            ]
        })),
    ));
    let dep_id = dbg
        .breakpoints
        .iter()
        .find(|bp| bp.line == 10)
        .map(|bp| bp.id)
        .expect("dep BP present");

    // With no trigger fired, hitting line 10 must skip.
    let vars: BTreeMap<String, harn_vm::VmValue> = BTreeMap::new();
    let action = dbg.classify_breakpoint_hit(10, &vars);
    assert!(
        matches!(action, BreakpointAction::Skip),
        "dep must skip pre-trigger"
    );

    // Fire the trigger (line 5). This arms id=1 in armed_breakpoints.
    let action = dbg.classify_breakpoint_hit(5, &vars);
    assert!(matches!(action, BreakpointAction::Stop));

    // Now the dep should stop on next hit.
    let action = dbg.classify_breakpoint_hit(10, &vars);
    assert!(
        matches!(action, BreakpointAction::Stop),
        "dep must arm after trigger fires"
    );

    // And stay armed even after enter_running would reset hit counts
    // within the same run — as long as armed_breakpoints retains its
    // record. enter_running itself clears armed; that's by design.
    dbg.enter_running();
    assert!(!dbg.armed_breakpoints.contains_key(&dep_id), "reset on run");
}

#[test]
fn test_set_function_breakpoints_stores_list_and_echoes_response() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(
        1,
        "setFunctionBreakpoints",
        Some(json!({
            "breakpoints": [
                { "name": "llm_call" },
                { "name": "host_run_pipeline", "condition": "model == \"gpt-5\"" },
                { "name": "do_work", "hitCondition": ">=3" }
            ]
        })),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(
        responses[0].command.as_deref(),
        Some("setFunctionBreakpoints")
    );
    assert_eq!(dbg.function_breakpoints.len(), 3);
    assert_eq!(dbg.function_breakpoints[0].name, "llm_call");
    assert_eq!(
        dbg.function_breakpoints[1].condition.as_deref(),
        Some("model == \"gpt-5\"")
    );
    assert_eq!(
        dbg.function_breakpoints[2].hit_condition.as_deref(),
        Some(">=3")
    );

    let body = responses[0].body.as_ref().unwrap();
    let verified = body["breakpoints"].as_array().unwrap();
    assert_eq!(verified.len(), 3);
    assert_eq!(verified[0]["verified"], true);
}

#[test]
fn test_set_function_breakpoints_replaces_prior_list_and_clears_hit_counts() {
    let mut dbg = Debugger::new();
    dbg.handle_message(make_request(
        1,
        "setFunctionBreakpoints",
        Some(json!({
            "breakpoints": [{ "name": "alpha" }, { "name": "beta" }]
        })),
    ));
    // Pretend a hit registered in the prior session.
    if let Some(fb) = dbg.function_breakpoints.first() {
        dbg.bp_hit_counts.insert(fb.id, 7);
    }
    dbg.handle_message(make_request(
        2,
        "setFunctionBreakpoints",
        Some(json!({ "breakpoints": [{ "name": "gamma" }] })),
    ));
    assert_eq!(dbg.function_breakpoints.len(), 1);
    assert_eq!(dbg.function_breakpoints[0].name, "gamma");
    assert!(
        dbg.bp_hit_counts.is_empty(),
        "hit counts must reset on edit"
    );
}

#[test]
fn test_function_breakpoint_fires_on_matching_call() {
    let mut dbg = Debugger::new();

    let (_dir, file) = write_temp_program(
        "fn_bp.harn",
        "fn helper() -> int { return 42 }\npipeline t(task) { let x = helper()\n log(x) }",
    );

    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));
    dbg.handle_message(make_request(
        3,
        "setFunctionBreakpoints",
        Some(json!({ "breakpoints": [{ "name": "helper" }] })),
    ));
    let mut responses = dbg.handle_message(make_request(4, "configurationDone", None));
    while dbg.is_running() && responses.len() < 50 {
        responses.extend(dbg.step_running_vm());
    }

    let stopped_on_fn = responses.iter().any(|r| {
        r.event.as_deref() == Some("stopped")
            && r.body
                .as_ref()
                .and_then(|b| b.get("reason"))
                .and_then(|v| v.as_str())
                == Some("function breakpoint")
    });
    assert!(
        stopped_on_fn,
        "function breakpoint on helper() must produce a stopped event"
    );
    drop(dbg);
}

#[test]
fn test_step_over_step_stops_on_next_step_boundary() {
    let mut dbg = Debugger::new();

    let (_dir, file) = write_temp_program(
        "step_boundary.harn",
        r#"
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  first_step(ctx)
  second_step(ctx)
}

@step(name: "first")
fn first_step(ctx) {
  log(ctx)
}

@step(name: "second")
fn second_step(ctx) {
  log(ctx)
}

pipeline test(task) {
  merge_captain("issue")
}
"#,
    );

    dbg.handle_message(make_request(1, "initialize", None));
    dbg.handle_message(make_request(
        2,
        "launch",
        Some(json!({"program": file.to_string_lossy()})),
    ));
    let mut responses = dbg.handle_message(make_request(3, "harnStepOverStep", None));
    while dbg.is_running() && responses.len() < 50 {
        responses.extend(dbg.step_running_vm());
    }

    let stopped_on_step = responses.iter().any(|r| {
        r.event.as_deref() == Some("stopped")
            && r.body.as_ref().is_some_and(|body| {
                body["reason"] == "step"
                    && body["description"]
                        .as_str()
                        .is_some_and(|description| description.contains("@step first_step"))
            })
    });
    assert!(
        stopped_on_step,
        "harnStepOverStep must stop at the next @step function"
    );
    drop(dbg);
}

#[test]
fn test_set_breakpoints_accepts_hit_condition_and_log_message() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(
        1,
        "setBreakpoints",
        Some(json!({
            "source": {"path": "test.harn"},
            "breakpoints": [{
                "line": 5,
                "hitCondition": ">=3",
                "logMessage": "iter={i} name={name}",
                "condition": "x > 0"
            }]
        })),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(dbg.breakpoints.len(), 1);
    let bp = &dbg.breakpoints[0];
    assert_eq!(bp.line, 5);
    assert_eq!(bp.hit_condition.as_deref(), Some(">=3"));
    assert_eq!(bp.log_message.as_deref(), Some("iter={i} name={name}"));
    assert_eq!(bp.condition.as_deref(), Some("x > 0"));
}

#[test]
fn test_set_breakpoints_clears_hit_counts() {
    // Editing a breakpoint in the gutter resets the hit counter so
    // re-arming a `>=3` breakpoint doesn't inherit the prior run's
    // tally and fire on the next hit.
    let mut dbg = Debugger::new();
    dbg.handle_message(make_request(
        1,
        "setBreakpoints",
        Some(json!({
            "source": {"path": "test.harn"},
            "breakpoints": [{"line": 5, "hitCondition": ">=3"}],
        })),
    ));
    // Poke the counter as if we'd taken two hits already.
    let bp_id = dbg.breakpoints.first().map(|bp| bp.id).expect("bp set");
    dbg.bp_hit_counts.insert(bp_id, 2);
    assert_eq!(dbg.breakpoint_hit_count(bp_id), 2);
    dbg.handle_message(make_request(
        2,
        "setBreakpoints",
        Some(json!({
            "source": {"path": "test.harn"},
            "breakpoints": [{"line": 5, "hitCondition": ">=3"}],
        })),
    ));
    assert!(
        dbg.bp_hit_counts.is_empty(),
        "hit counts must reset on edit"
    );
    assert_eq!(dbg.breakpoint_hit_count(bp_id), 0);
}

#[test]
fn test_logpoint_template_renders_literal_braces() {
    // Render escapes without a live VM — pure string formatting of
    // `\{` and `\}` happens before evaluation is attempted.
    let mut dbg = Debugger::new();
    let rendered = dbg
        .render_logpoint_template_for_tests("literal \\{x\\} before {missing}")
        .unwrap();
    // Escaped braces come through literally; the `{missing}` expression
    // evaluates through the VM fallback path and errors — the renderer
    // inlines the error text instead of failing the whole template.
    assert!(rendered.starts_with("literal {x} before <"));
    assert!(rendered.contains("no active VM session"));
}

#[test]
fn test_logpoint_template_errors_on_unclosed_brace() {
    let mut dbg = Debugger::new();
    let err = dbg
        .render_logpoint_template_for_tests("oops {still_open")
        .unwrap_err();
    assert!(err.contains("missing closing"));
}

#[test]
fn test_prompt_provenance_requires_prompt_id() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(
        1,
        "burin/promptProvenance",
        Some(json!({"outputOffset": 12})),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(false));
    assert!(responses[0].message.as_ref().unwrap().contains("promptId"));
}

#[test]
fn test_prompt_consumers_requires_template_uri() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(1, "burin/promptConsumers", Some(json!({}))));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(false));
    assert!(responses[0]
        .message
        .as_ref()
        .unwrap()
        .contains("templateUri"));
}

#[test]
fn test_prompt_consumers_returns_empty_list_for_unknown_template() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(
        1,
        "burin/promptConsumers",
        Some(json!({"templateUri": "/nope/does_not_exist.harn.prompt"})),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(true));
    let body = responses[0].body.as_ref().unwrap();
    let consumers = body["consumers"].as_array().unwrap();
    assert!(consumers.is_empty());
}

#[test]
fn test_set_variable_without_vm_returns_structured_error() {
    // No VM session yet — setVariable should fail with a helpful
    // message, not a panic. Full-VM integration is exercised via the
    // harn-vm unit tests that drive the same code path.
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(
        1,
        "setVariable",
        Some(json!({
            "variablesReference": 1,
            "name": "x",
            "value": "42"
        })),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(false));
    let msg = responses[0].message.as_ref().unwrap();
    assert!(msg.contains("no active VM session") || msg.contains("setVariable"));
}

#[test]
fn test_set_variable_rejects_missing_name() {
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(
        1,
        "setVariable",
        Some(json!({"variablesReference": 1, "value": "42"})),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(false));
    let msg = responses[0].message.as_ref().unwrap();
    assert!(msg.contains("missing"));
}

#[test]
fn test_set_expression_rejects_non_identifier_paths() {
    // Path-based assignment (dots, subscripts) is a follow-up; the
    // fast path must surface a clear error instead of silently no-oping.
    let mut dbg = Debugger::new();
    let responses = dbg.handle_message(make_request(
        1,
        "setExpression",
        Some(json!({"expression": "plan.tasks[0]", "value": "{}"})),
    ));
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0].success, Some(false));
    assert!(responses[0]
        .message
        .as_ref()
        .unwrap()
        .contains("bare identifiers"));
}

#[test]
fn test_hit_condition_matches_parses_all_forms() {
    use super::breakpoints::{check_condition_literal, hit_condition_matches};

    // Bare N → fire exactly on Nth hit.
    assert_eq!(hit_condition_matches("3", 2), Some(false));
    assert_eq!(hit_condition_matches("3", 3), Some(true));
    assert_eq!(hit_condition_matches("3", 4), Some(false));

    // >=N / >N / <N / <=N.
    assert_eq!(hit_condition_matches(">=5", 4), Some(false));
    assert_eq!(hit_condition_matches(">=5", 5), Some(true));
    assert_eq!(hit_condition_matches(">=5", 9), Some(true));
    assert_eq!(hit_condition_matches(">5", 5), Some(false));
    assert_eq!(hit_condition_matches("<3", 2), Some(true));
    assert_eq!(hit_condition_matches("<=3", 3), Some(true));
    assert_eq!(hit_condition_matches("==7", 7), Some(true));
    assert_eq!(hit_condition_matches("=2", 2), Some(true));

    // %N — every Nth hit, with 0 rejected as malformed.
    assert_eq!(hit_condition_matches("%4", 4), Some(true));
    assert_eq!(hit_condition_matches("%4", 8), Some(true));
    assert_eq!(hit_condition_matches("%4", 5), Some(false));
    assert_eq!(hit_condition_matches("%4", 0), Some(false));
    assert_eq!(hit_condition_matches("%0", 1), None);

    // Garbage → None so the caller can surface a diagnostic.
    assert_eq!(hit_condition_matches("hello", 1), None);
    assert_eq!(hit_condition_matches("= =1", 1), None);

    let mut vars = BTreeMap::new();
    vars.insert("count".to_string(), VmValue::Int(5));
    vars.insert(
        "label".to_string(),
        VmValue::String(std::sync::Arc::from("ready")),
    );

    assert_eq!(check_condition_literal("count >= 5", &vars), Ok(true));
    assert_eq!(
        check_condition_literal("label == \"ready\"", &vars),
        Ok(true)
    );
    assert!(check_condition_literal("label > 0", &vars)
        .unwrap_err()
        .contains("expected numeric variable"));
    assert!(check_condition_literal("missing", &vars)
        .unwrap_err()
        .contains("could not be evaluated"));
}

// ---------------------------------------------------------------
// Subagent ↔ DAP thread bridging (issue #1868)
// ---------------------------------------------------------------

mod subagent_bridge {
    use super::*;
    use harn_vm::agent_events::{AgentEvent, WorkerEvent};

    fn worker_update(
        worker_id: &str,
        worker_name: &str,
        event: WorkerEvent,
        parent: Option<&str>,
        suspend_reason: Option<&str>,
    ) -> AgentEvent {
        let mut metadata = serde_json::Map::new();
        if let Some(p) = parent {
            metadata.insert("parent_worker_id".to_string(), json!(p));
        }
        if let Some(r) = suspend_reason {
            metadata.insert("suspension".to_string(), json!({ "reason": r }));
        }
        AgentEvent::WorkerUpdate {
            session_id: "session-test".to_string(),
            worker_id: worker_id.to_string(),
            worker_name: worker_name.to_string(),
            worker_task: "demo".to_string(),
            worker_mode: "background".to_string(),
            event,
            status: event.as_status().to_string(),
            metadata: serde_json::Value::Object(metadata),
            audit: None,
        }
    }

    #[test]
    fn capabilities_advertise_new_lifecycle_filters() {
        let mut dbg = Debugger::new();
        let responses = dbg.handle_message(make_request(1, "initialize", None));
        let init = &responses[0];
        let filters = init.body.as_ref().unwrap()["exceptionBreakpointFilters"]
            .as_array()
            .unwrap();
        let filter_ids: Vec<&str> = filters
            .iter()
            .map(|f| f["filter"].as_str().unwrap())
            .collect();
        for required in [
            "break-on-suspend",
            "break-on-resume",
            "break-on-drain-decision",
        ] {
            assert!(
                filter_ids.contains(&required),
                "missing filter id {required}; have {filter_ids:?}"
            );
        }
    }

    #[test]
    fn set_exception_breakpoints_toggles_subagent_filter_flags() {
        let mut dbg = Debugger::new();
        assert!(!dbg.break_on_subagent_suspend);
        assert!(!dbg.break_on_subagent_resume);
        assert!(!dbg.break_on_drain_decision);

        dbg.handle_message(make_request(
            1,
            "setExceptionBreakpoints",
            Some(json!({
                "filters": ["break-on-suspend", "break-on-drain-decision"],
            })),
        ));
        assert!(dbg.break_on_subagent_suspend);
        assert!(!dbg.break_on_subagent_resume);
        assert!(dbg.break_on_drain_decision);

        // Clearing the filter list flips the toggles back off.
        dbg.handle_message(make_request(
            2,
            "setExceptionBreakpoints",
            Some(json!({ "filters": [] })),
        ));
        assert!(!dbg.break_on_subagent_suspend);
        assert!(!dbg.break_on_drain_decision);
    }

    #[test]
    fn drain_emits_thread_started_then_stopped_suspend() {
        let mut dbg = Debugger::new();
        // Simulate the sink getting these from the VM.
        dbg.subagent_tracker
            .upsert_thread("w-1", "researcher", None, "running");
        // Push two raw observations onto the queue.
        {
            let mut guard = dbg.subagent_tracker.inner_for_test();
            guard
                .pending
                .push(super::super::subagents::SubagentObservation {
                    worker_id: "w-1".to_string(),
                    worker_name: "researcher".to_string(),
                    event: WorkerEvent::WorkerSpawned,
                    status: "running".to_string(),
                    parent_worker_id: None,
                    suspend_reason: None,
                    suspension: None,
                });
            guard
                .pending
                .push(super::super::subagents::SubagentObservation {
                    worker_id: "w-1".to_string(),
                    worker_name: "researcher".to_string(),
                    event: WorkerEvent::WorkerSuspended,
                    status: "suspended".to_string(),
                    parent_worker_id: None,
                    suspend_reason: Some("awaiting human approval".to_string()),
                    suspension: Some(json!({
                        "handle": "w-1",
                        "reason": "awaiting human approval",
                        "conditions": {
                            "trigger": {"provider": "github", "kind": "comment"},
                            "timeout": {"minutes": 30},
                        },
                        "resume_by_mechanism": "trigger",
                        "suspended_at": "01978f25-0000-7000-8000-000000000000",
                        "initiator": "operator",
                    })),
                });
        }

        let events = dbg.drain_subagent_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.as_deref(), Some("thread"));
        let body0 = events[0].body.as_ref().unwrap();
        assert_eq!(body0["reason"], "started");
        let thread_id = body0["threadId"].as_i64().unwrap();
        assert!(thread_id >= 100, "subagent thread ids start at 100");

        assert_eq!(events[1].event.as_deref(), Some("stopped"));
        let body1 = events[1].body.as_ref().unwrap();
        assert_eq!(body1["reason"], "suspend");
        assert_eq!(body1["description"], "awaiting human approval");
        assert_eq!(body1["threadId"], thread_id);
        // Suspended subagents must not freeze the rest of the debug
        // session; the IDE keeps focus on what the user was looking at.
        assert_eq!(body1["allThreadsStopped"], false);
    }

    #[test]
    fn drain_resume_emits_continued_and_clears_suspend_reason() {
        let mut dbg = Debugger::new();
        let thread = dbg
            .subagent_tracker
            .upsert_thread("w-1", "researcher", None, "running");
        dbg.subagent_tracker
            .mark_suspended("w-1", Some("timer"), Some(json!({"reason": "timer"})));
        {
            let mut guard = dbg.subagent_tracker.inner_for_test();
            guard
                .pending
                .push(super::super::subagents::SubagentObservation {
                    worker_id: "w-1".to_string(),
                    worker_name: "researcher".to_string(),
                    event: WorkerEvent::WorkerResumed,
                    status: "running".to_string(),
                    parent_worker_id: None,
                    suspend_reason: None,
                    suspension: None,
                });
        }

        let events = dbg.drain_subagent_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("continued"));
        let body = events[0].body.as_ref().unwrap();
        assert_eq!(body["threadId"], thread.thread_id as i64);
        assert_eq!(body["allThreadsContinued"], false);

        // The threads response must now report `running` again, no
        // stale suspend reason hanging around.
        let snapshot = dbg.subagent_tracker.snapshot_threads();
        assert_eq!(snapshot[0].1.last_status, "running");
        assert_eq!(snapshot[0].1.suspend_reason, None);
    }

    #[test]
    fn drain_resume_with_break_on_resume_filter_pauses_main_thread() {
        let mut dbg = Debugger::new();
        dbg.break_on_subagent_resume = true;
        {
            let mut guard = dbg.subagent_tracker.inner_for_test();
            guard
                .pending
                .push(super::super::subagents::SubagentObservation {
                    worker_id: "w-1".to_string(),
                    worker_name: "researcher".to_string(),
                    event: WorkerEvent::WorkerResumed,
                    status: "running".to_string(),
                    parent_worker_id: None,
                    suspend_reason: None,
                    suspension: None,
                });
        }
        let events = dbg.drain_subagent_events();
        // continued event still fires for the subagent thread...
        assert!(events
            .iter()
            .any(|r| r.event.as_deref() == Some("continued")));
        // ...and the main-thread pause is deferred to the next
        // step_running_vm tick via pending_pause.
        assert!(
            dbg.pending_pause,
            "break-on-resume must arm pending_pause so the main thread stops on the next step"
        );
    }

    #[test]
    fn handle_threads_surfaces_subagent_lineage() {
        let mut dbg = Debugger::new();
        let parent = dbg
            .subagent_tracker
            .upsert_thread("w-1", "parent-agent", None, "running");
        let child =
            dbg.subagent_tracker
                .upsert_thread("w-2", "child-agent", Some("w-1"), "suspended");
        dbg.subagent_tracker.mark_suspended(
            "w-2",
            Some("escalated to operator"),
            Some(json!({"reason": "escalated to operator"})),
        );

        let responses = dbg.handle_message(make_request(1, "threads", None));
        let body = responses[0].body.as_ref().unwrap();
        let threads = body["threads"].as_array().unwrap();
        // main + 2 subagents
        assert_eq!(threads.len(), 3);

        let parent_row = threads
            .iter()
            .find(|t| t["id"] == parent.thread_id as i64)
            .expect("parent thread missing");
        assert_eq!(parent_row["name"], "parent-agent");
        assert_eq!(parent_row["status"], "running");
        assert!(parent_row.get("parentId").is_none());

        let child_row = threads
            .iter()
            .find(|t| t["id"] == child.thread_id as i64)
            .expect("child thread missing");
        assert_eq!(child_row["name"], "child-agent");
        assert_eq!(child_row["status"], "suspended");
        assert_eq!(child_row["parentId"], parent.thread_id as i64);
        assert_eq!(child_row["suspendReason"], "escalated to operator");
    }

    #[test]
    fn suspended_subagent_thread_surfaces_stack_scope_and_variables() {
        let mut dbg = Debugger::new();
        let thread =
            dbg.subagent_tracker
                .upsert_thread("w-2", "child-agent", Some("w-1"), "suspended");
        dbg.subagent_tracker.mark_suspended(
            "w-2",
            Some("waiting on review"),
            Some(json!({
                "handle": "w-2",
                "reason": "waiting on review",
                "conditions": {
                    "trigger": {"provider": "github", "kind": "comment"},
                    "timeout": {"minutes": 30},
                },
                "resume_by_mechanism": "trigger",
                "suspended_at": "01978f25-0000-7000-8000-000000000000",
                "initiator": "operator",
            })),
        );

        let stack = dbg.handle_message(make_request(
            1,
            "stackTrace",
            Some(json!({"threadId": thread.thread_id})),
        ));
        let frames = stack[0].body.as_ref().unwrap()["stackFrames"]
            .as_array()
            .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0]["name"], "<suspended: waiting on review>");
        let frame_id = frames[0]["id"].as_i64().unwrap();

        let scopes = dbg.handle_message(make_request(
            2,
            "scopes",
            Some(json!({"frameId": frame_id})),
        ));
        let scope = &scopes[0].body.as_ref().unwrap()["scopes"][0];
        assert_eq!(scope["name"], "Suspension");
        let variables_reference = scope["variablesReference"].as_i64().unwrap();

        let variables = dbg.handle_message(make_request(
            3,
            "variables",
            Some(json!({"variablesReference": variables_reference})),
        ));
        let vars = variables[0].body.as_ref().unwrap()["variables"]
            .as_array()
            .unwrap();
        let names: Vec<&str> = vars
            .iter()
            .map(|var| var["name"].as_str().unwrap())
            .collect();
        for required in [
            "handle",
            "conditions",
            "resume_by_mechanism",
            "suspended_at",
            "initiator",
        ] {
            assert!(names.contains(&required), "missing {required}: {vars:?}");
        }
        let conditions = vars
            .iter()
            .find(|var| var["name"] == "conditions")
            .expect("conditions variable");
        assert!(
            conditions["variablesReference"].as_i64().unwrap() > 0,
            "conditions should be expandable: {conditions:?}"
        );
    }

    #[test]
    fn drain_failed_emits_stopped_exception_then_thread_exited() {
        let mut dbg = Debugger::new();
        dbg.subagent_tracker
            .upsert_thread("w-1", "demo", None, "running");
        {
            let mut guard = dbg.subagent_tracker.inner_for_test();
            guard
                .pending
                .push(super::super::subagents::SubagentObservation {
                    worker_id: "w-1".to_string(),
                    worker_name: "demo".to_string(),
                    event: WorkerEvent::WorkerFailed,
                    status: "failed".to_string(),
                    parent_worker_id: None,
                    suspend_reason: Some("OOM in tool".to_string()),
                    suspension: None,
                });
        }
        let events = dbg.drain_subagent_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.as_deref(), Some("stopped"));
        assert_eq!(events[0].body.as_ref().unwrap()["reason"], "exception");
        assert_eq!(events[1].event.as_deref(), Some("thread"));
        assert_eq!(events[1].body.as_ref().unwrap()["reason"], "exited");
    }

    #[test]
    fn drain_terminal_worker_events_emit_thread_exited() {
        for (event, status) in [
            (WorkerEvent::WorkerCompleted, "completed"),
            (WorkerEvent::WorkerStopped, "stopped"),
            (WorkerEvent::WorkerCancelled, "cancelled"),
        ] {
            let mut dbg = Debugger::new();
            dbg.subagent_tracker
                .upsert_thread("w-1", "demo", None, "running");
            {
                let mut guard = dbg.subagent_tracker.inner_for_test();
                guard
                    .pending
                    .push(super::super::subagents::SubagentObservation {
                        worker_id: "w-1".to_string(),
                        worker_name: "demo".to_string(),
                        event,
                        status: status.to_string(),
                        parent_worker_id: None,
                        suspend_reason: None,
                        suspension: None,
                    });
            }
            let events = dbg.drain_subagent_events();
            assert_eq!(events.len(), 1, "{event:?} should emit one DAP event");
            assert_eq!(events[0].event.as_deref(), Some("thread"));
            assert_eq!(events[0].body.as_ref().unwrap()["reason"], "exited");
        }
    }

    #[test]
    fn drain_progressed_events_are_silent() {
        let mut dbg = Debugger::new();
        dbg.subagent_tracker
            .upsert_thread("w-1", "demo", None, "running");
        {
            let mut guard = dbg.subagent_tracker.inner_for_test();
            for _ in 0..5 {
                guard
                    .pending
                    .push(super::super::subagents::SubagentObservation {
                        worker_id: "w-1".to_string(),
                        worker_name: "demo".to_string(),
                        event: WorkerEvent::WorkerProgressed,
                        status: "progressed".to_string(),
                        parent_worker_id: None,
                        suspend_reason: None,
                        suspension: None,
                    });
            }
        }
        let events = dbg.drain_subagent_events();
        assert!(
            events.is_empty(),
            "WorkerProgressed must be silent on DAP; got {events:?}"
        );
    }

    #[test]
    fn subagent_sink_handle_event_pushes_into_tracker() {
        // Verify the SubagentEventSink correctly translates a
        // WorkerUpdate AgentEvent into an observation on the
        // tracker's queue. Bypasses the global wildcard-sink
        // registry (which is process-global and would otherwise
        // require test serialization to avoid cross-test leaks
        // under cargo's parallel test runner).
        use super::super::subagents::{SubagentEventSink, SubagentTracker};
        use harn_vm::agent_events::AgentEventSink;
        let tracker = SubagentTracker::new();
        let sink = SubagentEventSink::new(tracker.clone());
        sink.handle_event(&worker_update(
            "w-1",
            "alpha",
            WorkerEvent::WorkerSpawned,
            None,
            None,
        ));
        sink.handle_event(&worker_update(
            "w-1",
            "alpha",
            WorkerEvent::WorkerSuspended,
            None,
            Some("manual"),
        ));
        let drained = tracker.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].event, WorkerEvent::WorkerSpawned);
        assert_eq!(drained[1].event, WorkerEvent::WorkerSuspended);
        assert_eq!(drained[1].suspend_reason.as_deref(), Some("manual"));
    }
}
