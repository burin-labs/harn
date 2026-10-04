use super::*;

#[tokio::test(flavor = "current_thread")]
async fn unauthenticated_ingress_cannot_cancel_or_prepare_work() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let config = AcpServerConfig::new(None).with_auth_policy(AuthPolicy {
        methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig::single(
            "test-secret",
        ))],
        mcp_allowlist: None,
    });
    let mut server = AcpServer::new_with_output(config, AcpOutput::Channel(tx));
    let cancellation = SessionCancellation::default();
    let active = cancellation.prepare_prompt();
    let sid = format!("auth-stop-{}", uuid::Uuid::new_v4());
    server
        .session_cancellations
        .lock()
        .unwrap()
        .insert(sid.clone(), cancellation.clone());
    for method in [
        "session/cancel",
        "session/truncate",
        "session/close",
        "session/stop",
        "session/prompt",
    ] {
        let mut message = serde_json::json!({
            "jsonrpc": "2.0", "method": method, "params": {"sessionId": sid},
        });
        for request in [false, true] {
            if request {
                message["id"] = serde_json::json!(20);
            }
            assert!(matches!(
                prepare_session_request(
                    &server.session_cancellations,
                    &server.concurrent_controls,
                    &message
                ),
                PreparedSessionRequest::Unprepared
            ));
            assert!(!preempt_session_interruption(
                &server.session_cancellations,
                &server.concurrent_controls,
                &message
            ));
            server.handle_incoming_message(message.clone()).await;
            assert!(
                !active.cancelled.load(Ordering::SeqCst),
                "{method} must not mutate protected work"
            );
            if request {
                assert_eq!(
                    recv_json(&mut rx).await["error"]["code"],
                    ACP_AUTH_REQUIRED_CODE
                );
            } else {
                assert!(
                    rx.try_recv().is_err(),
                    "unauthenticated notification has no response"
                );
            }
        }
    }
    server
        .handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 21, "method": "authenticate",
            "params": {"methodId": "apiKey", "_meta": {"harn": {"apiKey": "test-secret"}}},
        }))
        .await;
    assert!(recv_json(&mut rx).await.get("result").is_some());
    let stop = serde_json::json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": sid}});
    assert!(preempt_session_interruption(
        &server.session_cancellations,
        &server.concurrent_controls,
        &stop
    ));
    assert!(
        active.cancelled.load(Ordering::SeqCst),
        "authenticated Stop reaches its registered work"
    );
    let fresh = cancellation.prepare_prompt();
    let mut request = stop;
    request["id"] = serde_json::json!(22);
    server.handle_incoming_message(request).await;
    assert!(fresh.cancelled.load(Ordering::SeqCst));
    assert_eq!(recv_json(&mut rx).await["result"]["status"], "cancelled");
}

#[test]
fn preparing_next_prompt_cannot_revive_cancelled_work() {
    let session = SessionCancellation::default();
    let old = session.prepare_prompt();
    assert!(session.cancel());
    let next = session.prepare_prompt();
    let queued_next = session.prepare_prompt();
    assert!(old.cancelled.load(Ordering::SeqCst));
    assert!(!next.cancelled.load(Ordering::SeqCst));
    assert!(!queued_next.cancelled.load(Ordering::SeqCst));
    assert!(session.cancel());
    assert!(old.cancelled.load(Ordering::SeqCst));
    assert!(next.cancelled.load(Ordering::SeqCst));
    assert!(queued_next.cancelled.load(Ordering::SeqCst));
    assert!(!session.cancel());
}

#[test]
fn stop_cancels_every_admitted_prompt_but_not_a_later_admission() {
    let session = SessionCancellation::default();
    let running = session.prepare_prompt();
    let queued = session.prepare_prompt();
    assert!(session.cancel());
    assert!(running.cancelled.load(Ordering::SeqCst));
    assert!(queued.cancelled.load(Ordering::SeqCst));
    let later = session.prepare_prompt();
    assert!(!later.cancelled.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "current_thread")]
async fn queued_cancel_acknowledgements_cannot_cancel_a_later_prompt() {
    for prompt_between_stops in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut server =
            AcpServer::new_with_output(AcpServerConfig::new(None), AcpOutput::Channel(tx));
        server.handle_incoming_message(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "session/new",
            "params": {"cwd": dir.path(), "environmentPolicy": {"kind": "isolated", "grants": []}},
        })).await;
        let sid = recv_json(&mut rx).await["result"]["sessionId"]
            .as_str()
            .unwrap()
            .to_string();
        let cancellation =
            lookup_session_cancellation(&server.session_cancellations, &sid).unwrap();
        let old = cancellation.prepare_prompt();
        let stop = |id| {
            serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "session/cancel", "params": {"sessionId": sid},
            })
        };
        let first = stop(20);
        let first_state = prepare_session_request(
            &server.session_cancellations,
            &server.concurrent_controls,
            &first,
        );
        let queued = prompt_between_stops.then(|| cancellation.prepare_prompt());
        let second = stop(21);
        let second_state = prepare_session_request(
            &server.session_cancellations,
            &server.concurrent_controls,
            &second,
        );
        let fresh = cancellation.prepare_prompt();

        server.handle_prepared_message(first, first_state).await;
        server.handle_prepared_message(second, second_state).await;

        assert!(old.cancelled.load(Ordering::SeqCst));
        if let Some(queued) = queued {
            assert!(queued.cancelled.load(Ordering::SeqCst));
        }
        assert!(
            !fresh.cancelled.load(Ordering::SeqCst),
            "a queued acknowledgement must not apply Stop again"
        );
        let first_ack = recv_response_with_id(&mut rx, 20).await;
        let second_ack = recv_response_with_id(&mut rx, 21).await;
        assert_eq!(first_ack["result"]["status"], "cancelled");
        assert_eq!(
            second_ack["result"]["status"],
            if prompt_between_stops {
                "cancelled"
            } else {
                "already_cancelled"
            }
        );
    }
}
