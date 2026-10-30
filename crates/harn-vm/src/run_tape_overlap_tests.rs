use super::*;

#[test]
fn sequential_dispatches_remain_replayable() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let args = serde_json::json!({});
    {
        let _guard = record_full(tmp.path());
        record_llm_call(&request("first"), &result("calling", Some("t1")));
        for id in ["t1", "t2"] {
            record_tool(id, &args, Ok(ok(serde_json::json!(id))));
        }
    }
    let workspace = tmp.path().join("branch");
    let replay = branch(tmp.path(), 1, &workspace);
    serve_llm_call(&request("first")).unwrap().unwrap();
    for id in ["t1", "t2"] {
        assert!(matches!(
            serve_tool_call(id, "write", &args),
            ToolServe::Served(_, _)
        ));
        assert_eq!(
            std::fs::read(workspace.join(format!("src/{id}.txt"))).unwrap(),
            b"written"
        );
    }
    assert!(serve_llm_call(&request("continuation")).is_none());
    assert_eq!(replay.finish().divergence, None);
}

#[test]
fn overlapping_dispatches_cannot_replay_ambiguous_file_effects() {
    let _lock = PROCESS_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let args = serde_json::json!({});
    {
        let _guard = record_full(tmp.path());
        record_llm_call(&request("first"), &result("calling", Some("t1")));
        let first = begin_tool_call("t1", "write", &args, false).unwrap();
        let second = begin_tool_call("t2", "write", &args, false).unwrap();
        finish_tool_call(second, Ok(ok(serde_json::json!("second"))));
        finish_tool_call(first, Ok(ok(serde_json::json!("first"))));
    }
    for id in ["t1", "t2"] {
        let workspace = tmp.path().join(format!("branch-{id}"));
        let replay = branch(tmp.path(), 1, &workspace);
        serve_llm_call(&request("first")).unwrap().unwrap();
        assert!(matches!(
            serve_tool_call(id, "write", &args),
            ToolServe::Diverged(_)
        ));
        assert!(replay
            .report()
            .divergence
            .unwrap()
            .detail
            .contains("overlapping"));
        assert!(!workspace.join(format!("src/{id}.txt")).exists());
        assert_eq!(replay.report().live_calls, 0);
    }
}
