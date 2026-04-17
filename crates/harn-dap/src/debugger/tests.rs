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
    use super::breakpoints::hit_condition_matches;

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
}
