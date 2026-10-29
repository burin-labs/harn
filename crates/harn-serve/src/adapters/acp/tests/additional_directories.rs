use super::*;

#[tokio::test(flavor = "current_thread")]
async fn standard_directory_grants_replace_durably_and_refuse_partial_lists() {
    use harn_session_store::SessionStore;
    harn_vm::reset_thread_local_state();
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let first = root.path().join("first");
    let second = root.path().join("second");
    for path in [&workspace, &first, &second] {
        std::fs::create_dir(path).unwrap();
    }
    let state = workspace.join("state");
    let launcher = harn_vm::security::LauncherEnvironment::from_snapshot(
        [("HARN_STATE_DIR".into(), state.to_str().unwrap().into())].into(),
    );
    let config = AcpServerConfig::new(None).with_launcher_environment(launcher);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(config, AcpOutput::Channel(tx));
    let directories =
        [second, first].map(|path| path.canonicalize().unwrap().to_str().unwrap().to_string());
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "session/new",
            "params": {"cwd": workspace, "additionalDirectories": directories,
                "environmentPolicy": {"kind": "isolated"}},
        }))
        .await;
    let created = recv_response_with_id(&mut rx, 1).await;
    let session = created["result"]["sessionId"]
        .as_str()
        .expect("admitted session");
    assert_eq!(
        created["result"]["session"]["additionalDirectories"],
        serde_json::json!(directories)
    );
    let scope = &server.sessions[session].store_scope;
    let store = scope
        .run(async { harn_vm::open_canonical_store(scope.workspace()) })
        .await
        .unwrap();
    assert_eq!(
        store
            .describe(session)
            .await
            .unwrap()
            .additional_directories()
            .unwrap(),
        directories
    );
    let before = harn_vm::agent_sessions::workspace_anchor(session).unwrap();
    let upstream_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/protocols/schemas/acp-1.26.0-upstream.schema.json");
    let upstream: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(upstream_path).unwrap()).unwrap();
    let schema = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$ref": "#/$defs/SessionInfo",
        "$defs": upstream["$defs"],
    });
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    let persisted = acp_persisted_session_item(store.describe(session).await.unwrap());
    for item in [created["result"]["session"].clone(), persisted] {
        assert!(validator.is_valid(&item), "upstream SessionInfo: {item}");
        let mut invalid = item;
        invalid["additionalDirectories"] = serde_json::json!([7]);
        assert!(
            !validator.is_valid(&invalid),
            "the upstream schema must reject numeric roots"
        );
    }
    for (id, invalid) in [
        serde_json::json!([directories[0], workspace.join("missing")]),
        serde_json::json!([7]),
        serde_json::Value::Null,
        serde_json::json!(["relative"]),
    ]
    .into_iter()
    .enumerate()
    {
        let id = id as u64 + 2;
        server
            .handle_incoming_message(serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "session/resume",
                "params": {"sessionId": session, "additionalDirectories": invalid},
            }))
            .await;
        let refused = recv_response_with_id(&mut rx, id).await;
        assert_eq!(refused["error"]["code"], -32602, "{refused}");
        assert_eq!(
            harn_vm::agent_sessions::workspace_anchor(session),
            Some(before.clone())
        );
        assert_eq!(
            store
                .describe(session)
                .await
                .unwrap()
                .additional_directories()
                .unwrap(),
            directories
        );
    }
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 6, "method": "session/resume",
            "params": {"sessionId": session},
        }))
        .await;
    let cleared = recv_response_with_id(&mut rx, 6).await;
    assert_eq!(
        cleared["result"]["session"]["additionalDirectories"],
        serde_json::json!([])
    );
    assert!(store
        .describe(session)
        .await
        .unwrap()
        .additional_directories()
        .unwrap()
        .is_empty());
    assert!(harn_vm::agent_sessions::workspace_anchor(session)
        .unwrap()
        .additional_roots
        .is_empty());

    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "session/close",
            "params": {"sessionId": session},
        }))
        .await;
    let closed = recv_response_with_id(&mut rx, 7).await;
    assert!(closed.get("result").is_some(), "{closed}");
    store
        .update(
            session,
            harn_session_store::UpdateSession {
                attributes: [(
                    "harn.workspace.additionalDirectories".into(),
                    serde_json::json!([7]),
                )]
                .into(),
                ..harn_session_store::UpdateSession::default()
            },
        )
        .await
        .unwrap();
    let metadata = store.describe(session).await.unwrap();
    let invalid = acp_persisted_session_item(metadata);
    assert_eq!(invalid["error"]["code"], "invalid_session_metadata");
    assert!(invalid.get("additionalDirectories").is_none());
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 8, "method": "session/list",
            "params": {"cwd": workspace},
        }))
        .await;
    let refused = recv_response_with_id(&mut rx, 8).await;
    assert_eq!(refused["error"]["code"], -32000, "{refused}");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("invalid additional directories"),
        "{refused}"
    );
}
