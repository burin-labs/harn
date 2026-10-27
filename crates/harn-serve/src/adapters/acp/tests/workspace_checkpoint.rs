use super::*;

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn acp_workspace_checkpoint_restores_attributed_edit_and_refuses_diverged_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(root.join("agent.txt"), "agent original").unwrap();
    std::fs::write(root.join("contested.txt"), "contested original").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "init"]);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": root.to_string_lossy(), "environmentPolicy": {"kind": "isolated", "grants": []}},
        }))
        .await;
    let session_id = recv_json(&mut rx).await["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    let mut next_id = 2;
    let mut call = |action: &str, extra: serde_json::Value| {
        let mut params = serde_json::json!({"sessionId": session_id, "action": action});
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        next_id += 1;
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": next_id,
            "method": "session/workspace_checkpoint",
            "params": params,
        })
    };

    server
        .handle_incoming_message(call("capture", serde_json::json!({"boundaryId": "turn-1"})))
        .await;
    assert_eq!(recv_json(&mut rx).await["result"]["available"], true);

    server
        .handle_incoming_message(call("call_begin", serde_json::json!({"callId": "edit-1"})))
        .await;
    assert_eq!(recv_json(&mut rx).await["result"]["available"], true);
    // Forking opens the live session store while the workspace call is active.
    // Its database and lock files must not become edits eligible for restore.
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 100, "method": "session/fork",
            "params": {"sessionId": session_id},
        }))
        .await;
    let fork = recv_response_with_id(&mut rx, 100).await;
    assert!(fork["result"]["sessionId"].is_string(), "{fork}");
    std::fs::write(root.join("agent.txt"), "agent edited, longer").unwrap();
    std::fs::write(root.join("contested.txt"), "contested by the agent").unwrap();
    server
        .handle_incoming_message(call(
            "call_end",
            serde_json::json!({"callId": "edit-1", "sideEffect": "workspace_write"}),
        ))
        .await;
    let attributed = recv_json(&mut rx).await;
    assert_eq!(attributed["result"]["changes"].as_array().unwrap().len(), 2);

    std::fs::write(root.join("contested.txt"), "contested by a person").unwrap();
    server
        .handle_incoming_message(call("restore", serde_json::json!({"boundaryId": "turn-1"})))
        .await;
    let receipt = recv_json(&mut rx).await;
    assert_eq!(receipt["result"]["restored"][0]["path"], "agent.txt");
    assert_eq!(receipt["result"]["refused"][0]["path"], "contested.txt");
    assert_eq!(
        std::fs::read_to_string(root.join("agent.txt")).unwrap(),
        "agent original"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("contested.txt")).unwrap(),
        "contested by a person"
    );
    assert!(root.join(".harn/session-store.sqlite").is_file());
    assert!(root.join(".harn/agent-run-writers/project.lock").is_file());

    // A malformed path filter is rejected, never widened to "restore everything".
    std::fs::write(root.join("agent.txt"), "edited again").unwrap();
    for bad in [
        serde_json::json!("agent.txt"),
        serde_json::json!([1]),
        serde_json::json!([]),
    ] {
        server
            .handle_incoming_message(call(
                "restore",
                serde_json::json!({"boundaryId": "turn-1", "paths": bad}),
            ))
            .await;
        assert_eq!(recv_json(&mut rx).await["error"]["code"], -32602);
    }
    assert_eq!(
        std::fs::read_to_string(root.join("agent.txt")).unwrap(),
        "edited again"
    );

    server
        .handle_incoming_message(call("bogus", serde_json::json!({})))
        .await;
    assert_eq!(recv_json(&mut rx).await["error"]["code"], -32602);
    harn_hostlib::workspace_checkpoint::drop_session(&session_id);
}
