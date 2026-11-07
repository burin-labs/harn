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
            outbound: Arc::default(),
            elicitations: Arc::default(),
            cancellation: SessionCancellation::default().prepare_prompt(),
            assistant_state: std::sync::Mutex::new(VisibleTextState::default()),
            host_responder: None,
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
        assert_eq!(bridge.outbound.pending_count().await, 1);
        bridge
            .outbound
            .complete(serde_json::json!({"id":request["id"],"result": "Proceed"}))
            .await;
        assert_eq!(call.await.unwrap(), serde_json::json!("Proceed"));
        assert_eq!(bridge.outbound.pending_count().await, 0);
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
    assert_eq!(bridge.outbound.pending_count().await, 0);
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
    assert_eq!(bridge.outbound.pending_count().await, 0);
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
    bridge.outbound.close().await;
    assert!(call
        .await
        .unwrap_err()
        .to_string()
        .contains("Client closed connection"));
    let after_disconnect = bridge
        .call_client_for_cleanup(
            "terminal/release",
            serde_json::json!({"terminalId":"closed"}),
        )
        .await;
    assert!(after_disconnect
        .unwrap_err()
        .to_string()
        .contains("Client closed connection"));
    assert_eq!(bridge.outbound.pending_count().await, 0);
    assert!(
        rx.try_recv().is_err(),
        "cleanup cannot register a request after disconnect"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_calls_cancel_the_exact_remote_request_and_retire_only_their_waiter() {
    let (bridge, mut rx) = bridge();
    let mut first =
        Box::pin(bridge.call_client("host/call", serde_json::json!({"name":"interaction.ask"})));
    let first_request = transmitted(&mut rx, first.as_mut()).await;
    let output = bridge.output.clone();
    let host = harn_vm::bridge::HostBridge::from_outbound_requests(
        bridge.outbound.clone(),
        Arc::new(move |line| {
            output.write_line(line);
            Ok(())
        }),
        harn_vm::bridge::HostBridgeControlState::new(
            bridge.cancellation.cancelled.clone(),
            bridge.cancellation.notify.clone(),
            Default::default(),
            harn_vm::tool_call_cancellations::fresh_registry(),
        ),
    );
    let second = host.call(
        "host/call",
        serde_json::json!({"name":"project.metadata_get"}),
    );
    tokio::pin!(second);
    let second_request = transmitted(&mut rx, second.as_mut()).await;
    assert_ne!(first_request["id"], second_request["id"]);
    assert_eq!(bridge.outbound.pending_count().await, 2);
    drop(first);
    let cancel: serde_json::Value = serde_json::from_str(&rx.recv().await.unwrap()).unwrap();
    assert_eq!(cancel["method"], ACP_METHOD_CANCEL_REQUEST);
    assert_eq!(cancel["params"]["requestId"], first_request["id"]);
    bridge
        .outbound
        .complete(serde_json::json!({"id":first_request["id"],"result":"late answer"}))
        .await;
    assert_eq!(
        bridge.outbound.pending_count().await,
        1,
        "a late response has no waiter"
    );
    bridge
        .outbound
        .complete(serde_json::json!({"id":second_request["id"],"result":"second answer"}))
        .await;
    assert_eq!(second.await.unwrap(), serde_json::json!("second answer"));
    assert_eq!(bridge.outbound.pending_count().await, 0);
    assert!(
        rx.try_recv().is_err(),
        "answered calls must not emit cancellation"
    );
}

struct HeldHost {
    entered: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl super::super::HostResponder for HeldHost {
    fn respond(
        &self,
        _: &str,
        _: &serde_json::Value,
    ) -> Result<serde_json::Value, super::super::AcpJsonRpcError> {
        self.entered
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(())
            .unwrap();
        let _ = self.release.lock().unwrap().recv();
        Ok(serde_json::json!("answer"))
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn in_process_host_deadlines_match_client_deadlines() {
    use futures::FutureExt;
    for (method, params, cleanup) in [
        ("host/capabilities", serde_json::json!({}), false),
        (
            "host/call",
            serde_json::json!({"name":"project.metadata_get"}),
            false,
        ),
        (
            "host/call",
            serde_json::json!({"name":"project.metadata_get"}),
            true,
        ),
    ] {
        let (mut bridge, _rx) = bridge();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        bridge.host_responder = Some(Arc::new(HeldHost {
            entered: std::sync::Mutex::new(Some(entered_tx)),
            release: std::sync::Mutex::new(release_rx),
        }));
        let call = bridge.call_client_inner(method, params, !cleanup);
        tokio::pin!(call);
        tokio::select! {
            entered = entered_rx => entered.unwrap(),
            result = &mut call => panic!("host request returned before responder started: {result:?}"),
        }
        let timeout = host_call_timeout(method);
        tokio::time::advance(timeout).await;
        let result = call.as_mut().now_or_never();
        // Release even on regression so runtime shutdown cannot hang on the worker.
        release_tx.send(()).unwrap();
        let error = result
            .expect("in-process responder must expire at the client deadline")
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            harn_vm::VmError::Runtime(format!(
                "Client did not respond to '{method}' within {timeout:?}"
            ))
            .to_string()
        );
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn in_process_human_answer_survives_deadline_and_stop_cancels_it() {
    use futures::FutureExt;
    let (mut bridge, _rx) = bridge();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    bridge.host_responder = Some(Arc::new(HeldHost {
        entered: std::sync::Mutex::new(Some(entered_tx)),
        release: std::sync::Mutex::new(release_rx),
    }));
    let call = bridge.call_client("host/call", serde_json::json!({"name":"interaction.ask"}));
    tokio::pin!(call);
    tokio::select! {
        entered = entered_rx => entered.unwrap(),
        result = &mut call => panic!("human request returned before responder started: {result:?}"),
    }
    tokio::time::advance(host_call_timeout("host/call") + Duration::from_secs(1)).await;
    let premature = call.as_mut().now_or_never();
    bridge.cancellation.cancel();
    assert!(premature.is_none(), "human answers must not expire");
    let result = call.await;
    release_tx.send(()).unwrap();
    assert!(result.unwrap_err().to_string().contains("Cancelled"));
}
