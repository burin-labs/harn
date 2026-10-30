use super::*;

#[tokio::test(flavor = "current_thread")]
async fn acp_websocket_session_list_discovers_live_and_detached_sessions_by_cwd() {
    let _guard = lock_harn_state_async().await;
    reset_active_event_log();

    let dir = tempdir().expect("tempdir");
    let cwd = dir.path().display().to_string();
    let canonical_cwd = dir.path().canonicalize().expect("canonical cwd");
    let (listener, _log, _dir) = start_acp_test_listener_with_project_root(
        ListenerRuntimeEnv::for_test().with_api_key("ws-test-key"),
        Some(dir.path()),
    )
    .await;
    let (mut owner, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("owner connect");
    let created = acp_request(&mut owner, 1, "session/new", isolated_env_at(&cwd)).await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_string();
    let roots = acp_request(
        &mut owner,
        2,
        "harn.session_workspace_roots",
        json!({"sessionId": session_id}),
    )
    .await;
    assert_eq!(
        roots["result"]["workspaceAnchor"]["primary"],
        json!(canonical_cwd.display().to_string()),
        "the session must advertise the anchor used by the list filter: {roots}"
    );

    let (mut observer, _) =
        tokio_tungstenite::connect_async(authorized_acp_request(listener.local_addr()))
            .await
            .expect("observer connect");
    let listed = acp_request(
        &mut observer,
        2,
        "session/list",
        json!({"cwd": cwd.clone()}),
    )
    .await;
    assert_eq!(
        listed["result"]["sessions"][0]["sessionId"],
        json!(session_id)
    );
    assert_eq!(listed["result"]["sessions"][0]["liveState"], json!("live"));
    assert_eq!(
        listed["result"]["sessions"][0]["attachableRoles"],
        json!(["observer", "controller"])
    );
    let anchored = acp_request(
        &mut observer,
        3,
        "session/list",
        json!({"workspaceAnchor": {"primary": cwd.clone()}}),
    )
    .await;
    assert_eq!(
        anchored["result"]["sessions"][0]["sessionId"],
        json!(session_id),
        "the documented workspace anchor must select the listener project: {anchored}"
    );

    owner.close(None).await.expect("close owner");
    drop(owner);
    wait_for_acp_session_detached(&listener, &session_id).await;

    let detached = acp_request(
        &mut observer,
        4,
        "session/list",
        json!({
            "cwd": cwd,
            "liveState": "detached_retained",
        }),
    )
    .await;
    assert_eq!(
        detached["result"]["sessions"][0]["sessionId"],
        json!(session_id)
    );
    assert_eq!(
        detached["result"]["sessions"][0]["attachableRoles"],
        json!(["host_owner", "observer"])
    );

    listener
        .shutdown(Duration::from_secs(5))
        .await
        .expect("shutdown listener");
    reset_active_event_log();
}
