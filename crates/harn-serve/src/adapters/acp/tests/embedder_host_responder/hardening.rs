use super::*;

struct ReleaseOnDrop(Option<std::sync::mpsc::Sender<()>>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

struct HeldBinder {
    dispatch_thread: std::thread::ThreadId,
    entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl HostResponderBinder for HeldBinder {
    fn bind(&self, root: &Path) -> Result<Arc<dyn HostResponder>, String> {
        assert_ne!(
            std::thread::current().id(),
            self.dispatch_thread,
            "binding must not run on the server thread"
        );
        self.entered
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(())
            .unwrap();
        self.release.lock().unwrap().recv().unwrap();
        FixtureBinder(Arc::default()).bind(root)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn slow_bind_keeps_router_controls_and_prompt_cancellation_live() {
    harn_clock::test_support::with_local_tasks(async {
        harn_vm::reset_thread_local_state();
        let root = tempfile::tempdir().unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = ReleaseOnDrop(Some(release_tx));
        let config = AcpServerConfig::new(None).with_host_responder(Arc::new(HeldBinder {
            dispatch_thread: std::thread::current().id(),
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(release_rx),
        }));
        let (tx, mut rx, server, session) =
            start_acp_channel_session_with_params(config, new_session_params(root.path())).await;
        tx.send(
            serde_json::json!({"jsonrpc":"2.0", "id":2, "method":"session/new",
            "params":new_session_params(root.path())}),
        )
        .unwrap();
        let other = recv_response_with_id(&mut rx, 2).await;
        let other = other["result"]["sessionId"].as_str().unwrap();
        tx.send(
            serde_json::json!({"jsonrpc":"2.0", "id":10, "method":"session/prompt",
            "params":{"sessionId":session, "prompt":[{"type":"text", "text":"assert(false)"}]}}),
        )
        .unwrap();
        harn_clock::test_support::within("bind reached blocking pool", entered_rx)
            .await
            .unwrap();
        tx.send(
            serde_json::json!({"jsonrpc":"2.0", "id":11, "method":"session/pending_prompts",
            "params":{"sessionId":other}}),
        )
        .unwrap();
        let response = recv_response_with_id(&mut rx, 11).await;
        assert!(response.get("result").is_some(), "{response}");
        tx.send(
            serde_json::json!({"jsonrpc":"2.0", "method":"session/cancel",
            "params":{"sessionId":session}}),
        )
        .unwrap();
        let cancelled = recv_response_with_id(&mut rx, 10).await;
        assert_eq!(
            cancelled["result"]["stopReason"], "cancelled",
            "{cancelled}"
        );
        drop(release);
        drop(tx);
        server.await.unwrap();
    })
    .await;
}

#[cfg(feature = "hostlib")]
#[tokio::test(flavor = "current_thread")]
async fn reanchor_keeps_host_calls_and_filesystem_on_the_turn_root() {
    harn_vm::reset_thread_local_state();
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let first_root = first.path().canonicalize().unwrap();
    let second_root = second.path().canonicalize().unwrap();
    std::fs::write(first_root.join("owner.txt"), "first").unwrap();
    std::fs::write(second_root.join("owner.txt"), "second").unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut server = AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
    server
        .handle_incoming_message(serde_json::json!({"jsonrpc":"2.0", "id":1,
        "method":"session/new", "params":new_session_params(first.path())}))
        .await;
    let created = recv_response_with_id(&mut rx, 1).await;
    let session_id = created["result"]["sessionId"].as_str().unwrap().to_owned();
    let (output_tx, _output_rx) = mpsc::unbounded_channel();
    let mut bridge = AcpBridge {
        client_capabilities: Default::default(),
        session_id: session_id.clone(),
        output: AcpOutput::Channel(output_tx),
        pending: Arc::default(),
        next_id_counter: AtomicU64::new(1),
        cancellation: SessionCancellation::default().prepare_prompt(),
        script_name: Mutex::default(),
        assistant_state: Mutex::default(),
        host_responder: Some(FixtureBinder(Arc::default()).bind(&first_root).unwrap()),
    };
    let session = server.sessions.get_mut(&session_id).unwrap();
    session.host_bridge = Some(Arc::new(
        harn_vm::bridge::HostBridge::from_parts_with_writer(
            Arc::default(),
            Arc::default(),
            Arc::new(|_| Ok(())),
            1,
        ),
    ));
    session.concurrent_control.set_prompt_active(true);
    server
        .handle_incoming_message(serde_json::json!({"jsonrpc":"2.0", "id":2,
        "method":"harn.session_reanchor", "params":{"sessionId":session_id, "path":second_root}}))
        .await;
    let changed = recv_response_with_id(&mut rx, 2).await;
    assert_eq!(changed["result"]["changed"], true, "{changed}");
    assert_eq!(
        harn_vm::agent_sessions::workspace_anchor(&session_id)
            .unwrap()
            .primary,
        second_root
    );

    for (expected_root, expected_text) in [(&first_root, "first"), (&second_root, "second")] {
        let host = bridge
            .call_client(
                "host/call",
                serde_json::json!({
            "sessionId":session_id, "name":"project.metadata_get", "args":{}}),
            )
            .await
            .unwrap();
        let fs_root = harn_hostlib::fs::configured_session_root(&session_id).unwrap();
        let registry = harn_hostlib::HostlibRegistry::new().with(harn_hostlib::fs::FsCapability);
        let read = registry.builtins().find("hostlib_fs_read_text").unwrap();
        let request = VmValue::dict(BTreeMap::from([
            (
                "path".to_string(),
                VmValue::String(fs_root.join("owner.txt").to_str().unwrap().into()),
            ),
            (
                "session_id".to_string(),
                VmValue::String(session_id.clone().into()),
            ),
        ]));
        let value = (read.handler)(&[request]).unwrap();
        assert_eq!(host["proof"]["root"], expected_root.to_str().unwrap());
        assert_eq!(value.as_dict().unwrap()["content"].display(), expected_text);
        assert_eq!(fs_root, *expected_root);

        server
            .clear_active_prompt_transport(&session_id)
            .await
            .unwrap();
        server
            .sync_session_root_from_workspace_anchor(&session_id)
            .unwrap();
        bridge.host_responder = Some(FixtureBinder(Arc::default()).bind(&second_root).unwrap());
    }
}
