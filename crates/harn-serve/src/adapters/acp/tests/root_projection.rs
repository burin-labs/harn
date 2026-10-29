#![cfg(unix)]

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn live_session_list_refuses_a_non_utf8_legacy_root_instead_of_hiding_the_session() {
    use std::os::unix::ffi::OsStringExt;
    harn_vm::reset_thread_local_state();
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let target = root
        .path()
        .join(std::ffi::OsString::from_vec(b"root-\xff".to_vec()));
    std::fs::create_dir(&target).unwrap();
    let link = root.path().join("readable-link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let launcher = harn_vm::security::LauncherEnvironment::from_snapshot(
        [(
            "HARN_STATE_DIR".into(),
            root.path().join("state").to_str().unwrap().into(),
        )]
        .into(),
    );
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(
        AcpServerConfig::new(None).with_launcher_environment(launcher),
        AcpOutput::Channel(tx),
    );
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc":"2.0", "id":1, "method":"session/new",
            "params":{"cwd":workspace,"environmentPolicy":{"kind":"isolated"}},
        }))
        .await;
    let created = recv_response_with_id(&mut rx, 1).await;
    let session = created["result"]["sessionId"].as_str().unwrap();
    harn_vm::agent_sessions::add_workspace_root(session, link.to_str().unwrap(), None, None)
        .unwrap();
    assert!(harn_vm::agent_sessions::workspace_anchor(session)
        .unwrap()
        .additional_directories()
        .is_err());
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc":"2.0", "id":2, "method":"session/list", "params":{"cwd":workspace},
        }))
        .await;
    let listed = recv_response_with_id(&mut rx, 2).await;
    assert_eq!(listed["error"]["code"], -32000, "{listed}");
    assert!(listed.get("result").is_none(), "{listed}");
    assert!(server.sessions.contains_key(session));
}
