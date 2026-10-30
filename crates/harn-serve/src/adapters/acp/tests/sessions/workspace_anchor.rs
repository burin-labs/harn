use super::*;

#[tokio::test(flavor = "current_thread")]
async fn acp_session_list_filters_by_workspace_anchor_and_cwd() {
    harn_clock::test_support::with_local_tasks(async {
            let dir = tempfile::tempdir().expect("tempdir");
            let primary = dir.path().join("project");
            let sibling = dir.path().join("project-tools");
            std::fs::create_dir_all(&primary).expect("primary dir");
            std::fs::create_dir_all(&sibling).expect("sibling dir");
            let canonical_primary = primary.canonicalize().expect("canonical primary");

            let (request_tx, request_rx) = mpsc::unbounded_channel();
            let (response_tx, mut response_rx) = mpsc::unbounded_channel();
            let server = tokio::task::spawn_local(super::super::run_acp_channel_server(
                AcpServerConfig::new(None),
                request_rx,
                response_tx,
            ));

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "session/new",
                    "params": {"cwd": primary.display().to_string(), "environmentPolicy": {"kind": "isolated", "grants": []}},
                }))
                .expect("send first session/new");
            let first = recv_json(&mut response_rx).await;
            let first_id = first["result"]["sessionId"]
                .as_str()
                .expect("first session id")
                .to_string();
            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/new",
                    "params": {"cwd": sibling.display().to_string(), "environmentPolicy": {"kind": "isolated", "grants": []}},
                }))
                .expect("send second session/new");
            let second = recv_json(&mut response_rx).await;
            let second_id = second["result"]["sessionId"]
                .as_str()
                .expect("second session id")
                .to_string();

            harn_vm::agent_sessions::set_workspace_anchor(
                &first_id,
                Some(harn_vm::workspace_anchor::WorkspaceAnchor {
                    primary: primary.clone(),
                    additional_roots: Vec::new(),
                    anchored_at: "2026-05-25T00:00:00Z".to_string(),
                }),
            )
            .expect("set first anchor");
            harn_vm::agent_sessions::set_workspace_anchor(
                &second_id,
                Some(harn_vm::workspace_anchor::WorkspaceAnchor {
                    primary: sibling.clone(),
                    additional_roots: Vec::new(),
                    anchored_at: "2026-05-25T00:00:00Z".to_string(),
                }),
            )
            .expect("set second anchor");

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "session/list",
                    "params": {
                        "workspaceAnchor": {"primary": primary.display().to_string()},
                    },
                }))
                .expect("send anchored session/list");
            let anchored = recv_json(&mut response_rx).await;
            assert_eq!(
                anchored["result"]["sessions"]
                    .as_array()
                    .expect("anchored sessions")
                    .len(),
                1
            );
            assert_eq!(
                anchored["result"]["sessions"][0]["sessionId"],
                serde_json::json!(first_id)
            );
            assert_eq!(
                anchored["result"]["sessions"][0]["workspaceAnchor"]["primary"],
                serde_json::json!(canonical_primary.display().to_string())
            );
            assert_eq!(
                anchored["result"]["sessions"][0]["_meta"]["harn"]["liveState"],
                serde_json::json!("live")
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "session/list",
                    "params": {"cwd": sibling.display().to_string()},
                }))
                .expect("send cwd session/list");
            let by_cwd = recv_json(&mut response_rx).await;
            assert_eq!(
                by_cwd["result"]["sessions"][0]["sessionId"],
                serde_json::json!(second_id)
            );

            request_tx
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 5,
                    "method": "session/load",
                    "params": {"sessionId": first_id},
                }))
                .expect("send session/load");
            let loaded = recv_json(&mut response_rx).await;
            assert_eq!(
                loaded["result"]["session"]["_meta"]["harn"]["workspaceAnchor"]["primary"],
                serde_json::json!(canonical_primary.display().to_string())
            );

            drop(request_tx);
            server.await.expect("ACP channel server task");
        }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_harn_workspace_anchor_methods_mutate_live_session() {
    harn_clock::test_support::with_local_tasks(async {
        harn_vm::reset_thread_local_state();
        let dir = tempfile::tempdir().expect("tempdir");
        let primary = dir.path().join("project");
        let sibling = dir.path().join("project-tools");
        let target = dir.path().join("target");
        std::fs::create_dir_all(&primary).expect("primary dir");
        std::fs::create_dir_all(&sibling).expect("sibling dir");
        std::fs::create_dir_all(&target).expect("target dir");
        let canonical_primary = primary.canonicalize().expect("canonical primary");
        let canonical_target = target.canonicalize().expect("canonical target");
        let canonical_sibling = sibling.canonicalize().expect("canonical sibling");

        let (request_tx, mut response_rx, server, session_id) =
            start_acp_channel_session_with_config(
                AcpServerConfig::new(None),
                serde_json::json!(primary.display().to_string()),
            )
            .await;

        request_tx
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "harn.session_workspace_roots",
                "params": {"sessionId": session_id},
            }))
            .expect("send roots request");
        let roots = recv_response_with_id(&mut response_rx, 2).await;
        assert_eq!(
            roots["result"]["workspaceAnchor"]["primary"],
            serde_json::json!(canonical_primary.display().to_string())
        );
        assert_eq!(
            harn_vm::agent_sessions::workspace_anchor(&session_id)
                .expect("live anchor")
                .primary,
            canonical_primary
        );

        request_tx
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "harn.session_add_root",
                "params": {
                    "sessionId": session_id,
                    "path": sibling.display().to_string(),
                    "mountMode": "extend",
                },
            }))
            .expect("send add-root request");
        let added = recv_response_with_id(&mut response_rx, 3).await;
        let additional = added["result"]["workspaceAnchor"]["additional_roots"]
            .as_array()
            .expect("additional roots");
        assert_eq!(additional.len(), 1);
        assert_eq!(
            additional[0]["path"],
            serde_json::json!(canonical_sibling.display().to_string())
        );
        assert_eq!(additional[0]["mount_mode"], serde_json::json!("extend"));

        request_tx
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "harn.session_reanchor",
                "params": {
                    "sessionId": session_id,
                    "path": target.display().to_string(),
                    "reason": "test reanchor",
                },
            }))
            .expect("send reanchor request");
        let reanchored = recv_response_with_id(&mut response_rx, 4).await;
        assert_eq!(reanchored["result"]["changed"], serde_json::json!(true));
        assert_eq!(
            reanchored["result"]["previousWorkspaceAnchor"]["primary"],
            serde_json::json!(canonical_primary.display().to_string())
        );
        assert_eq!(
            reanchored["result"]["workspaceAnchor"]["primary"],
            serde_json::json!(canonical_target.display().to_string())
        );

        let target_root = dir.path().join("target-tools");
        std::fs::create_dir_all(&target_root).expect("target root");
        let canonical_target_root = target_root.canonicalize().expect("canonical target root");
        request_tx
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "harn.session_add_root",
                "params": {
                    "sessionId": session_id,
                    "path": target_root.display().to_string(),
                    "mountMode": "extend",
                },
            }))
            .expect("send add-root after reanchor");
        let added_after_reanchor = recv_response_with_id(&mut response_rx, 6).await;
        assert!(
            added_after_reanchor.get("error").is_none(),
            "the captured store scope must still accept roots at the current anchor: {added_after_reanchor}"
        );
        let current_anchor = added_after_reanchor["result"]["workspaceAnchor"]
            .as_object()
            .expect("current workspace anchor");
        assert_eq!(
            current_anchor["primary"],
            serde_json::json!(canonical_target.display().to_string())
        );
        assert_eq!(
            current_anchor["additional_roots"][0]["path"],
            serde_json::json!(canonical_target_root.display().to_string())
        );

        request_tx
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "session/list",
                "params": {
                    "workspaceAnchor": {"primary": target.display().to_string()},
                },
            }))
            .expect("send filtered session/list");
        let listed = recv_response_with_id(&mut response_rx, 5).await;
        assert_eq!(
            listed["result"]["sessions"][0]["sessionId"],
            serde_json::json!(session_id)
        );
        assert_eq!(
            listed["result"]["sessions"][0]["workspaceAnchor"]["primary"],
            serde_json::json!(canonical_target.display().to_string())
        );

        request_tx.send(serde_json::json!({
            "jsonrpc": "2.0", "id": 7, "method": "session/resume",
            "params": {"sessionId": session_id,
                "additionalDirectories": [target_root.display().to_string()]},
        })).expect("resume reanchored live session without cwd");
        let resumed = recv_response_with_id(&mut response_rx, 7).await;
        assert!(resumed.get("error").is_none(), "reanchored resume: {resumed}");

        drop(request_tx);
        server.await.expect("ACP channel server task");
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn acp_harn_session_reanchor_seeds_missing_anchor_from_live_cwd() {
    harn_clock::test_support::with_local_tasks(async {
        harn_vm::reset_thread_local_state();
        let dir = tempfile::tempdir().expect("tempdir");
        let primary = dir.path().join("project");
        let target = dir.path().join("target");
        std::fs::create_dir_all(&primary).expect("primary dir");
        std::fs::create_dir_all(&target).expect("target dir");
        let canonical_primary = primary.canonicalize().expect("canonical primary");
        let canonical_target = target.canonicalize().expect("canonical target");

        let (request_tx, mut response_rx, server, session_id) =
            start_acp_channel_session_with_config(
                AcpServerConfig::new(None),
                serde_json::json!(primary.display().to_string()),
            )
            .await;

        request_tx
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "harn.session_reanchor",
                "params": {
                    "sessionId": session_id,
                    "path": target.display().to_string(),
                },
            }))
            .expect("send reanchor request");
        let reanchored = recv_response_with_id(&mut response_rx, 2).await;
        assert_eq!(
            reanchored["result"]["previousWorkspaceAnchor"]["primary"],
            serde_json::json!(canonical_primary.display().to_string())
        );
        assert_eq!(
            reanchored["result"]["workspaceAnchor"]["primary"],
            serde_json::json!(canonical_target.display().to_string())
        );

        drop(request_tx);
        server.await.expect("ACP channel server task");
    })
    .await;
}
