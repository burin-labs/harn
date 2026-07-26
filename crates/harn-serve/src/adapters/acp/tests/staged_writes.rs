use super::*;
use harn_hostlib::{
    tools::{permissions, ToolsCapability},
    BuiltinRegistry, HostlibCapability,
};
use harn_vm::VmDictExt;

async fn request_json(
    server: &mut AcpServer,
    rx: &mut mpsc::UnboundedReceiver<String>,
    request: serde_json::Value,
) -> serde_json::Value {
    server.handle_incoming_message(request).await;
    recv_json(rx).await
}

#[tokio::test(flavor = "current_thread")]
async fn acp_fs_mode_commit_and_discard_staged_hostlib_writes() {
    permissions::reset();
    permissions::enable_for_test();

    let dir = tempfile::TempDir::new().unwrap();
    let committed_file = dir.path().join("draft.txt");
    let discarded_file = dir.path().join("discard.txt");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));

    let created = request_json(
        &mut server,
        &mut rx,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": {"cwd": dir.path().to_string_lossy()},
        }),
    )
    .await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();

    let mode_response = request_json(
        &mut server,
        &mut rx,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/fs_mode",
            "params": {"sessionId": session_id.clone(), "mode": "staged"},
        }),
    )
    .await;
    assert_eq!(mode_response["result"]["previousMode"], "immediate");
    let mode_update = recv_json(&mut rx).await;
    assert_eq!(
        mode_update["params"]["update"]["_meta"]["harn"]["pendingCount"],
        0
    );

    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    let stage_write = |tool_call_id: &str, path: &std::path::Path, content: &str| {
        let _tool_call = harn_vm::agent_sessions::enter_current_tool_call(tool_call_id);
        let mut args = BTreeMap::new();
        args.put_str("session_id", session_id.as_str());
        args.put_str("path", path.to_string_lossy().as_ref());
        args.put_str("content", content);
        (registry
            .find("hostlib_tools_write_file")
            .expect("write_file builtin")
            .handler)(&[VmValue::dict(args)])
        .expect("stage write");
    };
    stage_write("tc-commit", &committed_file, "draft");
    stage_write("tc-discard", &discarded_file, "scratch");
    assert!(!committed_file.exists() && !discarded_file.exists());

    let staged_mode_response = request_json(
        &mut server,
        &mut rx,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 20,
            "method": "session/fs_mode",
            "params": {"sessionId": session_id.clone(), "mode": "staged"},
        }),
    )
    .await;
    assert_eq!(staged_mode_response["result"]["previousMode"], "staged");
    let staged_update = recv_json(&mut rx).await;
    let pending_writes = staged_update["params"]["update"]["_meta"]["harn"]["pendingWrites"]
        .as_array()
        .expect("pending writes");
    assert_eq!(
        pending_writes,
        &vec![
            serde_json::json!({
                "path": discarded_file.to_string_lossy(),
                "kind": "create",
                "byteDelta": 7,
                "snapshotId": "tc-discard",
            }),
            serde_json::json!({
                "path": committed_file.to_string_lossy(),
                "kind": "create",
                "byteDelta": 5,
                "snapshotId": "tc-commit",
            }),
        ]
    );

    let discard_response = request_json(
        &mut server,
        &mut rx,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/fs_discard_staged",
            "params": {
                "sessionId": session_id.clone(),
                "paths": [discarded_file.to_string_lossy()],
            },
        }),
    )
    .await;
    let discarded = &discard_response["result"]["discardedPaths"];
    let discarded_path = discarded[0].as_str().expect("discarded path");
    assert_eq!(std::path::Path::new(discarded_path), discarded_file);
    let discard_update = recv_json(&mut rx).await;
    let advertised_paths = discard_update["params"]["update"]["_meta"]["harn"]["pendingWrites"]
        .as_array()
        .expect("pending writes after discard")
        .iter()
        .map(|write| write["path"].as_str().expect("pending path").to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        advertised_paths,
        vec![committed_file.to_string_lossy().into_owned()]
    );

    let commit_response = request_json(
        &mut server,
        &mut rx,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/fs_commit_staged",
            "params": {
                "sessionId": session_id.clone(),
                "paths": advertised_paths,
            },
        }),
    )
    .await;
    assert_eq!(
        commit_response["result"]["committedPaths"][0],
        committed_file.to_string_lossy().as_ref()
    );
    assert_eq!(std::fs::read_to_string(&committed_file).unwrap(), "draft");
    let commit_update = recv_json(&mut rx).await;
    assert_eq!(
        commit_update["params"]["update"]["_meta"]["harn"]["kind"],
        "staged_writes_pending"
    );
    assert_eq!(
        commit_update["params"]["update"]["_meta"]["harn"]["pendingCount"],
        0
    );
}
