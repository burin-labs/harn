use super::*;

/// Every test that touches the process tape or branch holds this lock: both
/// are process-global by design.
static PROCESS_LOCK: Mutex<()> = Mutex::new(());

fn request(text: &str) -> LlmRequestPayload {
    LlmRequestPayload {
        provider: "anthropic".to_string(),
        model: "model-a".to_string(),
        messages: vec![serde_json::json!({"role": "user", "content": text})],
        system: Some("system".to_string()),
        max_tokens: 100,
        api_key: "secret-never-recorded".to_string(),
        ..LlmRequestPayload::default()
    }
}

fn result(text: &str, tool_call_id: Option<&str>) -> LlmResult {
    LlmResult {
        text: text.to_string(),
        tool_calls: tool_call_id
            .map(|id| vec![serde_json::json!({"id": id, "name": "write", "arguments": {}})])
            .unwrap_or_default(),
        model: "model-a".to_string(),
        provider: "anthropic".to_string(),
        ..LlmResult::default()
    }
}

fn ok_outcome(value: serde_json::Value) -> RecordedToolOutcome {
    RecordedToolOutcome {
        result: RecordedToolResult::Ok { value },
        executor: None,
        handler_outcome: Some("ok".to_string()),
    }
}

struct FixedObserver;

impl ToolEffectObserver for FixedObserver {
    fn begin(&self, _root: &Path, _call_id: &str) {}
    fn end(&self, _root: &Path, call_id: &str) -> EffectScan {
        EffectScan {
            changes: vec![(format!("src/{call_id}.txt"), Some(b"written".to_vec()))],
            incomplete: false,
        }
    }
}

/// Record a two-call run: call 1 asks for tool `t1`, call 2 finishes.
fn record_run(dir: &Path, workspace: &Path) {
    register_tool_effect_observer(Box::new(FixedObserver));
    let guard = start_recording(dir, RecordingLevel::Full, Some(workspace.to_path_buf()))
        .unwrap()
        .unwrap();
    record_llm_call(&request("first"), &result("calling", Some("t1")));
    let recording = begin_tool_call("t1").unwrap();
    finish_tool_call(
        recording,
        "write",
        &serde_json::json!({"path": "src/t1.txt"}),
        Ok(ok_outcome(serde_json::json!("done"))),
    );
    record_llm_call(&request("second"), &result("finished", None));
    assert_eq!(guard.error(), None);
}

#[test]
fn a_zero_edit_branch_serves_the_prefix_and_reapplies_tool_effects() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let (tape_dir, recorded_ws, branch_ws) = (
        tmp.path().join("tape"),
        tmp.path().join("ws"),
        tmp.path().join("branch"),
    );
    std::fs::create_dir_all(&branch_ws).unwrap();
    record_run(&tape_dir, &recorded_ws);

    let tape_text = std::fs::read_to_string(tape_dir.join(TAPE_FILE_NAME)).unwrap();
    assert!(!tape_text.contains("secret-never-recorded"));

    let branch = start_branch(BranchPlan {
        tape_path: tape_dir.join(TAPE_FILE_NAME),
        live_after_calls: 1,
        workspace_root: branch_ws.clone(),
    })
    .unwrap();
    let served = serve_llm_call(&request("first")).unwrap().unwrap();
    assert_eq!(served.text, "calling");
    assert_eq!(served.tool_calls[0]["id"], "t1");

    // The tool named by the served response returns its recorded outcome and
    // writes its recorded effect into the branch workspace without running.
    match serve_tool_call("t1", "write", &serde_json::json!({"path": "src/t1.txt"})) {
        ToolServe::Served(outcome) => assert_eq!(outcome, ok_outcome(serde_json::json!("done"))),
        _ => panic!("expected a served tool call"),
    }
    assert_eq!(
        std::fs::read(branch_ws.join("src/t1.txt")).unwrap(),
        b"written"
    );

    // Past K the call is live.
    assert!(serve_llm_call(&request("anything")).is_none());
    let report = branch.report();
    assert_eq!(
        (
            report.served_calls,
            report.served_tool_calls,
            report.live_calls
        ),
        (1, 1, 1)
    );
    assert_eq!(report.divergence, None);
}

#[test]
fn a_prefix_request_mismatch_fails_closed_and_never_goes_live() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let tape_dir = tmp.path().join("tape");
    record_run(&tape_dir, tmp.path());

    let branch = start_branch(BranchPlan {
        tape_path: tape_dir.join(TAPE_FILE_NAME),
        live_after_calls: 2,
        workspace_root: tmp.path().join("branch"),
    })
    .unwrap();
    let error = serve_llm_call(&request("edited")).unwrap().unwrap_err();
    let message = error.to_string();
    assert!(message.contains("model call 1"), "{message}");
    assert!(message.contains("messages[0]"), "{message}");
    // Every later call, including one past K, is refused rather than sent.
    assert!(serve_llm_call(&request("first")).unwrap().is_err());
    assert_eq!(branch.report().divergence.unwrap().call, 1);
}

#[test]
fn a_metadata_tape_refuses_to_branch() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    {
        let _guard = start_recording(tmp.path(), RecordingLevel::Metadata, None)
            .unwrap()
            .unwrap();
        record_llm_call(&request("first"), &result("done", None));
    }
    let error = start_branch(BranchPlan {
        tape_path: tmp.path().join(TAPE_FILE_NAME),
        live_after_calls: 1,
        workspace_root: tmp.path().to_path_buf(),
    })
    .err()
    .unwrap();
    assert!(error.contains("can be read but not branched"), "{error}");
}
