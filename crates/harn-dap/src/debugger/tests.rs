use std::collections::BTreeMap;

use harn_vm::VmValue;
use serde_json::json;

use super::state::{Debugger, StepMode};
use crate::protocol::DapMessage;

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
    assert_eq!(threads[0]["name"], "main");
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

    let dir = std::env::temp_dir().join("harn_dap_test");
    std::fs::create_dir_all(&dir).ok();
    let file = dir.join("test.harn");
    std::fs::write(&file, "pipeline test(task) { log(42) }").unwrap();

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

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir(&dir).ok();
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
    use std::rc::Rc;

    let mut dbg = Debugger::new();
    let mut inner = BTreeMap::new();
    inner.insert("bar".to_string(), VmValue::Int(99));
    dbg.variables
        .insert("foo".to_string(), VmValue::Dict(Rc::new(inner)));

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
    use std::rc::Rc;

    let mut dbg = Debugger::new();
    let mut inner = BTreeMap::new();
    inner.insert("c".to_string(), VmValue::String("deep".into()));
    let mut outer = BTreeMap::new();
    outer.insert("b".to_string(), VmValue::Dict(Rc::new(inner)));
    dbg.variables
        .insert("a".to_string(), VmValue::Dict(Rc::new(outer)));

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
    use std::rc::Rc;

    let mut dbg = Debugger::new();
    let mut map = BTreeMap::new();
    map.insert("key".to_string(), VmValue::Int(1));
    dbg.variables
        .insert("d".to_string(), VmValue::Dict(Rc::new(map)));

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
    assert_eq!(filters.len(), 1);
    assert_eq!(filters[0]["filter"], "all");
    assert_eq!(filters[0]["label"], "All Exceptions");
    assert_eq!(filters[0]["default"], false);
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

    let dir = std::env::temp_dir().join("harn_dap_bp_test");
    std::fs::create_dir_all(&dir).ok();
    let file = dir.join("test_bp.harn");
    std::fs::write(
        &file,
        "pipeline test(task) {\n  let x = 1\n  let y = 2\n  log(x + y)\n}",
    )
    .unwrap();

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

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir(&dir).ok();
}

#[test]
fn test_pause_interrupts_running_vm() {
    let mut dbg = Debugger::new();

    let dir = std::env::temp_dir().join("harn_dap_pause_test");
    std::fs::create_dir_all(&dir).ok();
    let file = dir.join("test_pause.harn");
    std::fs::write(
        &file,
        "pipeline test(task) {\n  let x = 1\n  let y = 2\n  let z = 3\n}",
    )
    .unwrap();

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

    std::fs::remove_file(&file).ok();
    std::fs::remove_dir(&dir).ok();
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
