use super::*;

#[test]
fn sensitive_store_keys_retain_redaction_context_in_recorded_payloads() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let marker = "synthetic-store-canary-without-a-token-pattern";
    let mut control = serde_json::json!({"api_key": marker});
    crate::redact::current_policy().redact_json_in_place(&mut control);
    assert!(!serde_json::to_string(&control).unwrap().contains(marker));
    {
        let _guard = record_full(dir.path());
        record_llm_call(&request("first"), &result("calling", Some("t1")));
        let mut tool = begin_tool_call("t1", "write", &serde_json::json!({}), false);
        harn_clock::test_support::current_thread_runtime().block_on(capture_transcript_changes(
            &mut tool,
            async {
                let mut store = crate::store::StoreState::at_state_root(&dir.path().join(".harn"));
                store
                    .set("api_key".to_string(), serde_json::json!(marker))
                    .unwrap();
                store
                    .set("padding".to_string(), serde_json::json!("x".repeat(8192)))
                    .unwrap();
            },
        ));
        finish_tool_call(tool.unwrap(), Ok(ok(serde_json::json!("done"))));
    }
    let tape = EventTape::load(&dir.path().join(TAPE_FILE_NAME)).unwrap();
    let mut measured = 0;
    for record in &tape.records {
        if let TapeRecordKind::ToolCall {
            store_changes: Some(payload),
            ..
        } = &record.kind
        {
            measured += 1;
            assert!(matches!(payload, TapePayload::Cas { .. }));
            let bytes = tape.resolve_payload(payload).unwrap();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(marker),
                "sensitive store value reached the tape payload"
            );
        }
    }
    assert_eq!(measured, 1);
    assert!(tape
        .records
        .iter()
        .any(|record| matches!(record.kind, TapeRecordKind::PayloadRedaction)));
    let result = start_branch(BranchPlan {
        tape,
        live_after_calls: 1,
        workspace_root: dir.path().to_path_buf(),
        state_root: dir.path().join("branch-state"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    });
    let Err(error) = result else {
        panic!("redacted values cannot establish an exact branch")
    };
    assert!(error.contains("redact"), "{error}");
}

#[test]
fn a_worker_store_write_refuses_instead_of_claiming_zero_captured_changes() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let store_root = dir.path().join("worker-store");
    {
        let _guard = record_full(dir.path());
        record_llm_call(&request("first"), &result("calling", Some("t1")));
        let mut tool = begin_tool_call("t1", "write", &serde_json::json!({}), false);
        harn_clock::test_support::current_thread_runtime().block_on(capture_transcript_changes(
            &mut tool,
            async {
                let root = store_root.clone();
                crate::runtime_stack::spawn(move || {
                    crate::store::StoreState::at_state_root(&root)
                        .set("worker".to_string(), serde_json::json!("observed"))
                })
                .join()
                .unwrap()
                .unwrap();
            },
        ));
        assert!(tool.as_ref().unwrap().store_changes.is_empty());
        let written: serde_json::Value = serde_json::from_slice(
            &std::fs::read(store_root.join(crate::runtime_paths::STORE_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(written["worker"], "observed");
        finish_tool_call(tool.unwrap(), Ok(ok(serde_json::json!("done"))));
    }
    let workspace = dir.path().join("branch");
    let replay = branch(dir.path(), 1, &workspace);
    serve_llm_call(&request("first")).unwrap().unwrap();
    assert!(matches!(
        serve_tool_call("t1", "write", &serde_json::json!({})),
        ToolServe::Diverged(_)
    ));
    assert!(replay
        .report()
        .divergence
        .unwrap()
        .detail
        .contains("outside the recorded dispatch context"));
    assert!(!workspace.join("src/t1.txt").exists());
}

#[test]
fn version_five_remains_readable_but_cannot_claim_store_mutation_evidence() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    record_run(dir.path());
    let path = dir.path().join(TAPE_FILE_NAME);
    let mut tape = EventTape::load(&path).unwrap();
    tape.header.version = 5;
    tape.persist(&path).unwrap();
    let loaded = EventTape::load(&path).expect("older tapes remain inspectable");
    let result = start_branch(BranchPlan {
        tape: loaded,
        live_after_calls: 1,
        workspace_root: dir.path().to_path_buf(),
        state_root: dir.path().join("branch-state"),
        host_state_roots: Vec::new(),
        prompt_edits: Vec::new(),
    });
    let Err(error) = result else {
        panic!("older runtime evidence cannot qualify exact replay")
    };
    assert!(error.contains("runtime store evidence"), "{error}");
}

#[test]
fn a_failed_store_write_makes_the_dispatch_unservable() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, "a file cannot be a store directory").unwrap();
    {
        let _guard = record_full(dir.path());
        record_llm_call(&request("first"), &result("calling", Some("t1")));
        let mut tool = begin_tool_call("t1", "write", &serde_json::json!({}), false);
        let error = harn_clock::test_support::current_thread_runtime()
            .block_on(capture_transcript_changes(&mut tool, async {
                crate::store::StoreState::at_state_root(&blocked)
                    .set("failed".to_string(), serde_json::json!(42))
            }))
            .unwrap_err();
        finish_tool_call(
            tool.unwrap(),
            Ok(RecordedToolResult::Runtime {
                message: error.to_string(),
            }),
        );
    }
    let workspace = dir.path().join("branch");
    let replay = branch(dir.path(), 1, &workspace);
    serve_llm_call(&request("first")).unwrap().unwrap();
    assert!(matches!(
        serve_tool_call("t1", "write", &serde_json::json!({})),
        ToolServe::Diverged(_)
    ));
    assert!(replay
        .report()
        .divergence
        .unwrap()
        .detail
        .contains("runtime store write did not persist"));
    assert!(!workspace.join("src/t1.txt").exists());
}

#[test]
fn unmeasured_store_changes_refuse_before_any_tool_effect_is_applied() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = tempfile::tempdir().unwrap();
    record_run(dir.path());
    let path = dir.path().join(TAPE_FILE_NAME);
    let mut tape = EventTape::load(&path).unwrap();
    let mut measured_tools = 0;
    for record in &mut tape.records {
        if let TapeRecordKind::ToolCall { store_changes, .. } = &mut record.kind {
            assert!(
                store_changes.take().is_some(),
                "new recordings measure even zero changes"
            );
            measured_tools += 1;
        }
    }
    assert_eq!(measured_tools, 1);
    tape.persist(&path).unwrap();
    let workspace = dir.path().join("branch");
    let replay = branch(dir.path(), 1, &workspace);
    serve_llm_call(&request("first")).unwrap().unwrap();
    assert!(matches!(
        serve_tool_call("t1", "write", &serde_json::json!({"path": "src/t1.txt"})),
        ToolServe::Diverged(_)
    ));
    assert!(!workspace.join("src/t1.txt").exists());
    let report = replay.report();
    assert_eq!(report.served_tool_calls, 0);
    assert_eq!(report.live_calls, 0);
    assert!(report
        .divergence
        .unwrap()
        .detail
        .contains("did not measure runtime store"));
}

#[test]
fn recorded_store_paths_must_stay_inside_the_branch_state_root() {
    for entry in [
        serde_json::json!({}),
        serde_json::json!({"first": 1, "second": 2}),
    ] {
        let bytes = serde_json::to_vec(&serde_json::json!([{
            "store": "store.json", "operation": {"kind": "set", "entry": entry}
        }]))
        .unwrap();
        assert!(crate::store::decode_dispatch(&bytes).is_err());
    }
    for store in [
        "",
        "../outside",
        "/outside",
        "workspace/../../outside",
        "events.sqlite",
        "workspace/other.json",
    ] {
        let bytes = serde_json::to_vec(&serde_json::json!([{
            "store": store, "operation": {"kind": "clear"}
        }]))
        .unwrap();
        assert!(crate::store::decode_dispatch(&bytes).is_err(), "{store}");
    }
    let bytes = serde_json::to_vec(&serde_json::json!([{
        "store": "workspace/.harn/store.json", "operation": {"kind": "clear"}
    }]))
    .unwrap();
    assert_eq!(crate::store::decode_dispatch(&bytes).unwrap().len(), 1);
}
