use super::*;

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
fn test_breakpoint_stop() {
    let mut dbg = Debugger::new();

    let (_dir, file) = write_temp_program(
        "test_bp.harn",
        "pipeline test(harness: Harness, task: unknown) {\n  const x = 1\n  const y = 2\n  harness.stdio.log(x + y)\n}",
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
        "pipeline test(harness: Harness, task: unknown) {\n  const x = 1\n  const y = 2\n  harness.stdio.log(x + y)\n}\n",
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
fn test_triggered_breakpoint_skipped_until_trigger_fires() {
    use super::super::breakpoints::BreakpointAction;

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
    let vars: harn_vm::value::DictMap = Default::default();
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
        "fn helper() -> int { return 42 }\npipeline t(harness: Harness, task: unknown) { const x = helper()\n harness.stdio.log(x) }",
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
fn test_hit_condition_matches_parses_all_forms() {
    use super::super::breakpoints::{check_condition_literal, hit_condition_matches};

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

    let mut vars = harn_vm::value::DictMap::new();
    vars.insert(harn_vm::value::intern_key("count"), VmValue::Int(5));
    vars.put_str("label", "ready");

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
