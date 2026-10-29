use super::*;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

fn bridge() -> (AcpBridge, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    (
        AcpBridge {
            client_capabilities: Default::default(),
            session_id: "human-wait-session".into(),
            output: AcpOutput::Channel(tx),
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id_counter: AtomicU64::new(1),
            cancellation: SessionCancellation::default().prepare_prompt(),
            script_name: std::sync::Mutex::new(String::new()),
            assistant_state: std::sync::Mutex::new(VisibleTextState::default()),
        },
        rx,
    )
}

async fn transmitted<F>(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    call: Pin<&mut F>,
) -> serde_json::Value
where
    F: Future<Output = Result<serde_json::Value, harn_vm::VmError>>,
{
    tokio::select! {
        frame = rx.recv() => serde_json::from_str(&frame.expect("actual host request")).unwrap(),
        result = call => panic!("host call returned before dispatch: {result:?}"),
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn human_calls_remain_pending_then_deliver_the_actual_answer() {
    let mut cases = vec![(
        "session/request_permission",
        serde_json::json!({"sessionId": "human-wait-session", "toolCall": {}, "options": []}),
    )];
    cases.extend(["interaction.ask", "interaction.ask_user", "interaction.request_approval", "permission.request"]
        .map(|operation| ("host/call", serde_json::json!({"sessionId": "human-wait-session", "name": operation, "args": {}}))));
    for (method, params) in cases {
        let (bridge, mut rx) = bridge();
        let expected = params.clone();
        let call = bridge.call_client(method, params);
        tokio::pin!(call);
        let request = transmitted(&mut rx, call.as_mut()).await;
        assert_eq!(request["method"], method);
        assert_eq!(request["params"], expected);
        tokio::time::advance(Duration::from_secs(301)).await;
        tokio::select! {
            result = &mut call => panic!("human response expired: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }
        assert_eq!(bridge.pending.lock().await.len(), 1);
        bridge
            .pending
            .lock()
            .await
            .remove(&request["id"].as_u64().unwrap())
            .unwrap()
            .send(serde_json::json!({"result": "Proceed"}))
            .unwrap();
        assert_eq!(call.await.unwrap(), serde_json::json!("Proceed"));
        assert!(bridge.pending.lock().await.is_empty());
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ordinary_host_calls_still_expire() {
    let (bridge, mut rx) = bridge();
    let call = bridge.call_client(
        "host/call",
        serde_json::json!({"name": "interaction.ask_other", "args": {}}),
    );
    tokio::pin!(call);
    let request = transmitted(&mut rx, call.as_mut()).await;
    assert_eq!(request["params"]["name"], "interaction.ask_other");
    tokio::time::advance(Duration::from_secs(301)).await;
    assert!(call
        .await
        .unwrap_err()
        .to_string()
        .contains("did not respond"));
    assert!(bridge.pending.lock().await.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn stop_settles_a_human_call_after_the_deadline_window() {
    let (bridge, mut rx) = bridge();
    let call = bridge.call_client(
        "host/call",
        serde_json::json!({"name": "interaction.ask", "args": {}}),
    );
    tokio::pin!(call);
    let _ = transmitted(&mut rx, call.as_mut()).await;
    tokio::time::advance(Duration::from_secs(301)).await;
    tokio::select! {
        result = &mut call => panic!("human response expired before Stop: {result:?}"),
        _ = tokio::task::yield_now() => {}
    }
    bridge.cancellation.cancelled.store(true, Ordering::SeqCst);
    bridge.cancellation.notify.notify_waiters();
    assert!(call.await.unwrap_err().to_string().contains("Cancelled"));
    assert!(bridge.pending.lock().await.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn disconnect_settles_a_human_call_after_the_deadline_window() {
    let (bridge, mut rx) = bridge();
    let call = bridge.call_client(
        "host/call",
        serde_json::json!({"name": "interaction.ask", "args": {}}),
    );
    tokio::pin!(call);
    let _ = transmitted(&mut rx, call.as_mut()).await;
    tokio::time::advance(Duration::from_secs(301)).await;
    tokio::select! {
        result = &mut call => panic!("human response expired before disconnect: {result:?}"),
        _ = tokio::task::yield_now() => {}
    }
    bridge.pending.lock().await.clear();
    assert!(call
        .await
        .unwrap_err()
        .to_string()
        .contains("Client closed connection"));
}
