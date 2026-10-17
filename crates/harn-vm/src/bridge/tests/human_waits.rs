use super::*;

#[test]
fn test_timeout_duration() {
    assert_eq!(
        bridge_call_timeout("host/work", &serde_json::json!({}), DEFAULT_TIMEOUT),
        Some(DEFAULT_TIMEOUT)
    );
    assert_eq!(DEFAULT_TIMEOUT.as_secs(), 300);
}

#[test]
fn only_declared_human_calls_remove_the_deadline() {
    let ordinary = Duration::from_secs(7);
    for (method, params) in [
        (
            "host/call",
            serde_json::json!({"name": "runtime.read_resource"}),
        ),
        (
            "host/call",
            serde_json::json!({"name": "interaction.ask_other"}),
        ),
        (
            "host/call",
            serde_json::json!({"name": "permission.request_other"}),
        ),
        ("host/call", serde_json::json!({"name": 3})),
        ("host/call", serde_json::json!({})),
        (
            "host/capabilities",
            serde_json::json!({"name": "interaction.ask"}),
        ),
    ] {
        assert_eq!(
            bridge_call_timeout(method, &params, ordinary),
            Some(ordinary)
        );
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn human_host_calls_survive_timeout_window_under_paused_time() {
    for operation in [
        "interaction.ask",
        "interaction.ask_user",
        "interaction.request_approval",
        "permission.request",
    ] {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let frames = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let captured = frames.clone();
        let bridge = HostBridge::from_parts_with_writer(
            pending.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |line| {
                captured
                    .lock()
                    .unwrap()
                    .push(serde_json::from_str(line).unwrap());
                Ok(())
            }),
            1,
        );
        let call = bridge.call(
            "host/call",
            serde_json::json!({"name": operation, "args": {"question": "Continue?"}}),
        );
        tokio::pin!(call);
        wait_for_pending(&pending, 1, call.as_mut()).await;
        assert_eq!(frames.lock().unwrap()[0]["params"]["name"], operation);
        tokio::time::advance(DEFAULT_TIMEOUT + Duration::from_secs(1)).await;
        tokio::select! {
            result = &mut call => panic!("human input {operation} expired: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }
        assert_eq!(pending.lock().await.len(), 1);
        pending
            .lock()
            .await
            .remove(&1)
            .unwrap()
            .send(serde_json::json!({"result": "Proceed"}))
            .unwrap();
        assert_eq!(call.await.unwrap(), serde_json::json!("Proceed"));
        assert!(pending.lock().await.is_empty());
    }
}
