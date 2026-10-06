use super::*;
use harn_clock::test_support::within;
use harn_parser::diagnostic_codes::Code;

fn test_bridge() -> HostBridge {
    HostBridge::from_parts(
        Arc::new(Mutex::new(HashMap::new())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(std::sync::Mutex::new(())),
        1,
    )
}

fn test_bridge_sharing_injection_state(owner: &HostBridge) -> HostBridge {
    HostBridge::from_parts_with_writer_and_control(
        Arc::new(Mutex::new(HashMap::new())),
        Arc::new(|_| Ok(())),
        100,
        HostBridgeControlState::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(Notify::new()),
            owner.injection_state(),
            crate::tool_call_cancellations::fresh_registry(),
        ),
    )
}

#[test]
fn test_json_rpc_request_format() {
    let request = crate::jsonrpc::request(
        1,
        "llm_call",
        serde_json::json!({
            "prompt": "Hello",
            "system": "Be helpful",
        }),
    );
    let s = serde_json::to_string(&request).unwrap();
    assert!(s.contains("\"jsonrpc\":\"2.0\""));
    assert!(s.contains("\"id\":1"));
    assert!(s.contains("\"method\":\"llm_call\""));
}

#[test]
fn test_json_rpc_notification_format() {
    let notification =
        crate::jsonrpc::notification("output", serde_json::json!({"text": "[harn] hello\n"}));
    let s = serde_json::to_string(&notification).unwrap();
    assert!(s.contains("\"method\":\"output\""));
    assert!(!s.contains("\"id\""));
}

#[test]
fn test_json_rpc_error_response_parsing() {
    let response = crate::jsonrpc::error_response(1, -32600, "Invalid request");
    assert!(response.get("error").is_some());
    assert_eq!(
        response["error"]["message"].as_str().unwrap(),
        "Invalid request"
    );
}

#[test]
fn test_json_rpc_success_response_parsing() {
    let response = crate::jsonrpc::response(
        1,
        serde_json::json!({
            "text": "Hello world",
            "input_tokens": 10,
            "output_tokens": 5,
        }),
    );
    assert!(response.get("result").is_some());
    assert_eq!(response["result"]["text"].as_str().unwrap(), "Hello world");
}

#[test]
fn test_cancelled_flag() {
    let cancelled = Arc::new(AtomicBool::new(false));
    assert!(!cancelled.load(Ordering::SeqCst));
    cancelled.store(true, Ordering::SeqCst);
    assert!(cancelled.load(Ordering::SeqCst));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn pending_permission_calls_return_when_cancellation_arrives() {
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let bridge = HostBridge::from_parts_with_writer(
        pending.clone(),
        cancelled.clone(),
        Arc::new(|_| Ok(())),
        1,
    );

    let call = bridge.call(
        crate::llm::acp_permission::METHOD_REQUEST_PERMISSION,
        serde_json::json!({}),
    );
    tokio::pin!(call);
    wait_for_pending(&pending, 1, call.as_mut()).await;

    cancelled.store(true, Ordering::SeqCst);
    bridge.cancel_notify.notify_waiters();

    let result = within("pending permission call observing cancellation", call).await;
    assert!(matches!(
        result,
        Err(VmError::Runtime(message)) if message.contains("cancelled")
    ));
    assert!(pending.lock().await.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn registered_cancel_wait_survives_notification_before_first_poll() {
    let notify = Notify::new();
    let wait = notify.notified();
    tokio::pin!(wait);
    wait.as_mut().enable();

    notify.notify_waiters();

    tokio::select! {
        () = &mut wait => {}
        _ = tokio::task::yield_now() => panic!("registered cancellation notification was lost"),
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn bridge_call_cannot_register_after_disconnect_clear() {
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let bridge = HostBridge::from_parts_with_writer(
        pending.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(|_| Ok(())),
        1,
    );
    let guard = pending.lock().await;
    let call = bridge.call(
        crate::llm::acp_permission::METHOD_REQUEST_PERMISSION,
        serde_json::json!({}),
    );
    tokio::pin!(call);
    tokio::select! {
        result = &mut call => panic!("call bypassed pending lock: {result:?}"),
        _ = tokio::task::yield_now() => {}
    }

    bridge.disconnected.store(true, Ordering::SeqCst);
    drop(guard);

    let result = call.await;
    assert!(matches!(
        result,
        Err(VmError::Runtime(message)) if message.contains("already closed")
    ));
    assert!(pending.lock().await.is_empty());
}

#[test]
fn call_progress_hides_non_user_visible_deltas() {
    let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let captured = lines.clone();
    let bridge = HostBridge::from_parts_with_writer(
        Arc::new(Mutex::new(HashMap::new())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(move |line| {
            captured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line.to_string());
            Ok(())
        }),
        1,
    );

    bridge.send_call_start(
        "call-1",
        "llm",
        "llm_call",
        serde_json::json!({"stream_publicly": true}),
    );
    bridge.send_call_progress(
        "call-1",
        r#"{"verdict":"done","reasoning":"internal"}"#,
        1,
        false,
    );

    let lines = lines.lock().unwrap_or_else(|e| e.into_inner());
    let progress: serde_json::Value =
        serde_json::from_str(&lines[1]).expect("call_progress notification json");
    let content = &progress["params"]["update"]["content"];
    assert_eq!(
        content["delta"],
        r#"{"verdict":"done","reasoning":"internal"}"#
    );
    assert_eq!(content["user_visible"], false);
    assert_eq!(content["visible_text"], "");
    assert_eq!(content["visible_delta"], "");
}

#[test]
fn call_progress_hides_non_public_streams() {
    let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let captured = lines.clone();
    let bridge = HostBridge::from_parts_with_writer(
        Arc::new(Mutex::new(HashMap::new())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(move |line| {
            captured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line.to_string());
            Ok(())
        }),
        1,
    );

    bridge.send_call_start(
        "call-1",
        "llm",
        "llm_call",
        serde_json::json!({"stream_publicly": false}),
    );
    bridge.send_call_progress("call-1", "secret schema bytes", 1, true);

    let lines = lines.lock().unwrap_or_else(|e| e.into_inner());
    let progress: serde_json::Value =
        serde_json::from_str(&lines[1]).expect("call_progress notification json");
    let content = &progress["params"]["update"]["content"];
    assert_eq!(content["delta"], "secret schema bytes");
    assert_eq!(content["user_visible"], true);
    assert_eq!(content["visible_text"], "");
    assert_eq!(content["visible_delta"], "");
}

#[test]
fn queued_messages_are_filtered_by_delivery_mode() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let bridge = test_bridge();
        bridge
            .push_queued_user_message("first".to_string(), "finish_step")
            .await;
        bridge
            .push_queued_user_message("second".to_string(), "audit_only")
            .await;

        let finish_step = bridge.take_queued_user_messages(false, true, false).await;
        assert_eq!(finish_step.len(), 1);
        assert_eq!(finish_step[0].content, "first");

        let audit_only = bridge.take_queued_user_messages(false, false, true).await;
        assert_eq!(audit_only.len(), 1);
        assert_eq!(audit_only[0].content, "second");
    });
}

#[test]
fn pending_user_messages_support_revoke_replace_and_delivery_states() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let bridge = test_bridge();
        let first_id = bridge
            .push_pending_user_message(
                "first".to_string(),
                serde_json::json!("first"),
                "audit_only",
            )
            .await;
        let second_id = bridge
            .push_pending_user_message(
                "second".to_string(),
                serde_json::json!("second"),
                "audit_only",
            )
            .await;

        assert_eq!(
            bridge
                .replace_pending_user_message(
                    &second_id,
                    "second edited".to_string(),
                    serde_json::json!("second edited"),
                )
                .await,
            PendingUserMessageMutationResult::Mutated
        );
        assert_eq!(
            bridge.revoke_pending_user_message(&first_id).await,
            PendingUserMessageMutationResult::Mutated
        );
        assert_eq!(
            bridge.revoke_pending_user_message(&first_id).await,
            PendingUserMessageMutationResult::AlreadyRevoked
        );

        let delivered = bridge
            .take_queued_user_messages_for(DeliveryCheckpoint::EndOfInteraction)
            .await;
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].message_id, second_id);
        assert_eq!(delivered[0].content, "second edited");

        assert_eq!(
            bridge.revoke_pending_user_message(&second_id).await,
            PendingUserMessageMutationResult::AlreadyDelivered
        );
        assert_eq!(
            bridge
                .replace_pending_user_message(
                    &second_id,
                    "too late".to_string(),
                    serde_json::json!("too late"),
                )
                .await,
            PendingUserMessageMutationResult::AlreadyDelivered
        );
        assert_eq!(
            bridge.revoke_pending_user_message("missing").await,
            PendingUserMessageMutationResult::UnknownMessageId
        );
    });
}

#[test]
fn pending_user_message_replace_preserves_fifo_position_and_mode() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let bridge = test_bridge();
        let first_id = bridge
            .push_pending_user_message(
                "first".to_string(),
                serde_json::json!("first"),
                "finish_step",
            )
            .await;
        let second_id = bridge
            .push_pending_user_message(
                "second".to_string(),
                serde_json::json!("second"),
                "finish_step",
            )
            .await;
        assert_eq!(
            bridge
                .replace_pending_user_message(
                    &first_id,
                    "first edited".to_string(),
                    serde_json::json!("first edited"),
                )
                .await,
            PendingUserMessageMutationResult::Mutated
        );

        let delivered = bridge
            .take_queued_user_messages_for(DeliveryCheckpoint::AfterCurrentOperation)
            .await;
        assert_eq!(
            delivered
                .iter()
                .map(|message| (&message.message_id, message.content.as_str(), message.mode))
                .collect::<Vec<_>>(),
            vec![
                (&first_id, "first edited", QueuedUserMessageMode::FinishStep,),
                (&second_id, "second", QueuedUserMessageMode::FinishStep),
            ]
        );
    });
}

#[test]
fn pending_user_message_state_survives_bridge_replacement() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let bridge = test_bridge();
        let revoked_id = bridge
            .push_pending_user_message(
                "revoke me".to_string(),
                serde_json::json!("revoke me"),
                "audit_only",
            )
            .await;
        let delivered_id = bridge
            .push_pending_user_message(
                "deliver me".to_string(),
                serde_json::json!("deliver me"),
                "audit_only",
            )
            .await;
        assert_eq!(
            bridge.revoke_pending_user_message(&revoked_id).await,
            PendingUserMessageMutationResult::Mutated
        );
        bridge.cancelled.store(true, Ordering::SeqCst);

        let replacement_bridge = test_bridge_sharing_injection_state(&bridge);
        assert_eq!(
            replacement_bridge
                .revoke_pending_user_message(&revoked_id)
                .await,
            PendingUserMessageMutationResult::AlreadyRevoked
        );
        let delivered = replacement_bridge
            .take_queued_user_messages_for(DeliveryCheckpoint::EndOfInteraction)
            .await;
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].message_id, delivered_id);
        assert_eq!(delivered[0].content, "deliver me");
        assert_eq!(
            bridge.revoke_pending_user_message(&delivered_id).await,
            PendingUserMessageMutationResult::AlreadyDelivered
        );
    });
}

#[test]
fn queued_transcript_injections_preserve_user_reminder_separation() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let bridge = test_bridge();
        bridge
            .push_queued_user_message("human follow-up".to_string(), "finish_step")
            .await;
        let reminder_id = bridge
            .push_queued_session_remind_from_params(&serde_json::json!({
                "body": "Host-provided ambient context.",
                "tags": ["host"],
                "dedupe_key": "host-context",
                "ttl_turns": 2,
                "mode": "audit_only",
                "_meta": {"harn": {"source": "test"}},
            }))
            .await
            .expect("valid reminder");

        let finish_step = bridge.take_queued_user_messages(false, true, false).await;
        assert_eq!(finish_step.len(), 1);
        assert_eq!(finish_step[0].content, "human follow-up");

        let no_user_messages = bridge.take_queued_user_messages(false, false, true).await;
        assert!(no_user_messages.is_empty());

        let injections = bridge
            .take_queued_transcript_injections_for(DeliveryCheckpoint::EndOfInteraction)
            .await;
        assert_eq!(injections.len(), 1);
        let QueuedTranscriptInjection::Reminder(reminder) = &injections[0] else {
            panic!("expected queued reminder");
        };
        assert_eq!(reminder.reminder.id, reminder_id);
        assert_eq!(reminder.reminder.body, "Host-provided ambient context.");
        assert_eq!(reminder.reminder.tags, vec!["host".to_string()]);
        assert_eq!(
            reminder.reminder.dedupe_key.as_deref(),
            Some("host-context")
        );
        assert_eq!(reminder.reminder.ttl_turns, Some(2));
        assert_eq!(
            reminder.reminder.source,
            crate::llm::helpers::ReminderSource::Bridge
        );
    });
}

#[test]
fn pending_injections_list_user_messages_and_reminders_in_fifo_order() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let bridge = test_bridge();
        let message_id = bridge
            .push_pending_user_message(
                "human follow-up".to_string(),
                serde_json::json!([{"type": "text", "text": "human follow-up"}]),
                "finish_step",
            )
            .await;
        let reminder_id = bridge
            .push_queued_session_remind_from_params(&serde_json::json!({
                "id": "rem-test",
                "body": "Host reminder",
                "tags": ["host"],
                "dedupe_key": "host-reminder",
                "ttl_turns": 2,
                "mode": "interrupt_immediate",
            }))
            .await
            .expect("valid session/remind payload");

        let pending = bridge.pending_injections_json().await;
        assert_eq!(pending["pendingCount"], 2);
        assert_eq!(pending["injections"][0]["kind"], "user");
        assert_eq!(pending["injections"][0]["id"], message_id);
        assert_eq!(pending["injections"][0]["messageId"], message_id);
        assert_eq!(pending["injections"][0]["mode"], "finish_step");
        assert_eq!(pending["injections"][0]["position"], 0);
        assert_eq!(pending["injections"][1]["kind"], "reminder");
        assert_eq!(pending["injections"][1]["id"], reminder_id);
        assert_eq!(pending["injections"][1]["reminderId"], "rem-test");
        assert_eq!(pending["injections"][1]["mode"], "interrupt_immediate");
        assert_eq!(pending["injections"][1]["body"], "Host reminder");
        assert_eq!(pending["injections"][1]["dedupeKey"], "host-reminder");
        assert_eq!(pending["injections"][1]["ttlTurns"], 2);
        assert_eq!(pending["injections"][1]["position"], 1);
    });
}

#[test]
fn pending_reminders_support_revoke_and_delivery_states() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let bridge = test_bridge();
        let revoked_id = bridge
            .push_queued_session_remind_from_params(&serde_json::json!({
                "id": "rem-revoke",
                "body": "remove me",
                "mode": "finish_step",
            }))
            .await
            .expect("valid session/remind payload");
        let delivered_id = bridge
            .push_queued_session_remind_from_params(&serde_json::json!({
                "id": "rem-deliver",
                "body": "deliver me",
                "mode": "finish_step",
            }))
            .await
            .expect("valid session/remind payload");

        assert_eq!(
            bridge.revoke_pending_reminder(&revoked_id).await,
            PendingReminderMutationResult::Mutated
        );
        assert_eq!(
            bridge.revoke_pending_reminder(&revoked_id).await,
            PendingReminderMutationResult::AlreadyRevoked
        );

        let pending = bridge.pending_injections_json().await;
        assert_eq!(pending["pendingCount"], 1);
        assert_eq!(pending["injections"][0]["reminderId"], delivered_id);

        let delivered = bridge
            .take_queued_transcript_injections_for(DeliveryCheckpoint::AfterCurrentOperation)
            .await;
        assert_eq!(delivered.len(), 1);
        let QueuedTranscriptInjection::Reminder(reminder) = &delivered[0] else {
            panic!("expected delivered reminder");
        };
        assert_eq!(reminder.reminder.id, delivered_id);

        assert_eq!(
            bridge.revoke_pending_reminder(&delivered_id).await,
            PendingReminderMutationResult::AlreadyDelivered
        );
        assert_eq!(
            bridge.revoke_pending_reminder("missing").await,
            PendingReminderMutationResult::UnknownReminderId
        );
    });
}

#[test]
fn bridge_remind_modes_honor_delivery_checkpoints() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let cases = [
            (
                "interrupt_immediate",
                DeliveryCheckpoint::InterruptImmediate,
                DeliveryCheckpoint::AfterCurrentOperation,
            ),
            (
                "finish_step",
                DeliveryCheckpoint::AfterCurrentOperation,
                DeliveryCheckpoint::EndOfInteraction,
            ),
            (
                "audit_only",
                DeliveryCheckpoint::EndOfInteraction,
                DeliveryCheckpoint::InterruptImmediate,
            ),
        ];

        for (mode, expected_checkpoint, wrong_checkpoint) in cases {
            let bridge = test_bridge();
            bridge
                .push_queued_session_remind_from_params(&serde_json::json!({
                    "body": format!("Reminder for {mode}"),
                    "mode": mode,
                }))
                .await
                .expect("valid session/remind payload");

            let premature = bridge
                .take_queued_transcript_injections_for(wrong_checkpoint)
                .await;
            assert!(
                premature.is_empty(),
                "{mode} reminder must not be delivered at {wrong_checkpoint:?}"
            );

            let delivered = bridge
                .take_queued_transcript_injections_for(expected_checkpoint)
                .await;
            assert_eq!(delivered.len(), 1, "{mode} reminder was not delivered");
            let QueuedTranscriptInjection::Reminder(reminder) = &delivered[0] else {
                panic!("expected reminder for {mode}");
            };
            assert_eq!(reminder.reminder.body, format!("Reminder for {mode}"));
        }
    });
}

#[test]
fn session_remind_validation_rejects_user_message_shape() {
    let err = queued_session_remind_from_params(&serde_json::json!({
        "content": "this is still a user message",
        "mode": "interrupt_immediate",
    }))
    .expect_err("session/remind must require a reminder body");
    assert!(err.contains(Code::ReminderInvalidShape.as_str()));
    assert!(err.contains("body"));
}

#[test]
fn session_remind_validation_rejects_unknown_options_separately() {
    let err = queued_session_remind_from_params(&serde_json::json!({
        "body": "valid body",
        "unknown_host_field": true,
    }))
    .expect_err("session/remind must reject unknown top-level fields");
    assert!(err.contains(Code::ReminderUnknownOption.as_str()));
    assert!(err.contains("unknown_host_field"));
}

#[test]
fn session_remind_validation_rejects_unknown_propagate_with_specific_code() {
    let err = queued_session_remind_from_params(&serde_json::json!({
        "body": "valid body",
        "propagate": "workspace",
    }))
    .expect_err("session/remind must reject unknown propagate values");
    assert!(err.contains(Code::ReminderUnknownPropagate.as_str()));
    assert!(err.contains("propagate"));
}

#[test]
fn test_json_result_to_vm_value_string() {
    let val = serde_json::json!("hello");
    let vm_val = json_result_to_vm_value(&val);
    assert_eq!(vm_val.display(), "hello");
}

#[test]
fn test_json_result_to_vm_value_dict() {
    let val = serde_json::json!({"name": "test", "count": 42});
    let vm_val = json_result_to_vm_value(&val);
    let VmValue::Dict(d) = &vm_val else {
        unreachable!("Expected Dict, got {:?}", vm_val);
    };
    assert_eq!(d.get("name").unwrap().display(), "test");
    assert_eq!(d.get("count").unwrap().display(), "42");
}

#[test]
fn test_json_result_to_vm_value_null() {
    let val = serde_json::json!(null);
    let vm_val = json_result_to_vm_value(&val);
    assert!(matches!(vm_val, VmValue::Nil));
}

#[test]
fn test_json_result_to_vm_value_nested() {
    let val = serde_json::json!({
        "text": "response",
        "tool_calls": [
            {"id": "tc_1", "name": "read_file", "arguments": {"path": "foo.rs"}}
        ],
        "input_tokens": 100,
        "output_tokens": 50,
    });
    let vm_val = json_result_to_vm_value(&val);
    let VmValue::Dict(d) = &vm_val else {
        unreachable!("Expected Dict, got {:?}", vm_val);
    };
    assert_eq!(d.get("text").unwrap().display(), "response");
    let VmValue::List(list) = d.get("tool_calls").unwrap() else {
        unreachable!("Expected List for tool_calls");
    };
    assert_eq!(list.len(), 1);
}

#[test]
fn parse_host_tools_list_accepts_object_wrapper() {
    let tools = parse_host_tools_list_response(serde_json::json!({
        "tools": [
            {
                "name": "Read",
                "description": "Read a file",
                "schema": {"type": "object"},
            }
        ]
    }))
    .expect("tool list");

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "Read");
    assert_eq!(tools[0]["deprecated"], false);
}

#[test]
fn parse_host_tools_list_accepts_compat_fields() {
    let tools = parse_host_tools_list_response(serde_json::json!({
        "result": {
            "tools": [
                {
                    "name": "Edit",
                    "short_description": "Apply an edit",
                    "input_schema": {"type": "object"},
                    "deprecated": true,
                }
            ]
        }
    }))
    .expect("tool list");

    assert_eq!(tools[0]["description"], "Apply an edit");
    assert_eq!(tools[0]["schema"]["type"], "object");
    assert_eq!(tools[0]["deprecated"], true);
}

#[test]
fn parse_host_tools_list_requires_tool_names() {
    let err = parse_host_tools_list_response(serde_json::json!({
        "tools": [
            {"description": "missing name"}
        ]
    }))
    .expect_err("expected error");
    assert!(err
        .to_string()
        .contains("host/tools/list: every tool must include a string `name`"));
}

#[test]
fn test_timeout_duration() {
    assert_eq!(bridge_call_timeout("host/work"), Some(DEFAULT_TIMEOUT));
    assert_eq!(DEFAULT_TIMEOUT.as_secs(), 300);
}

#[test]
fn interactive_permission_requests_have_no_bridge_timeout() {
    assert_eq!(
        bridge_call_timeout(crate::llm::acp_permission::METHOD_REQUEST_PERMISSION),
        None
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn non_interactive_bridge_calls_timeout_under_paused_time() {
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let bridge = HostBridge::from_parts_with_writer(
        pending.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(|_| Ok(())),
        1,
    );

    let call = bridge.call("host/work", serde_json::json!({}));
    tokio::pin!(call);
    wait_for_pending(&pending, 1, call.as_mut()).await;

    tokio::time::advance(DEFAULT_TIMEOUT).await;
    let result = call.await;
    assert!(matches!(
        result,
        Err(VmError::Runtime(message)) if message.contains("host/work")
            && message.contains("within 300s")
    ));
    assert!(pending.lock().await.is_empty());
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn permission_bridge_calls_survive_timeout_window_under_paused_time() {
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let bridge = HostBridge::from_parts_with_writer(
        pending.clone(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(|_| Ok(())),
        1,
    );

    let call = bridge.call(
        crate::llm::acp_permission::METHOD_REQUEST_PERMISSION,
        serde_json::json!({}),
    );
    tokio::pin!(call);
    wait_for_pending(&pending, 1, call.as_mut()).await;

    tokio::time::advance(DEFAULT_TIMEOUT + Duration::from_secs(1)).await;
    tokio::select! {
        result = &mut call => panic!("permission request timed out: {result:?}"),
        _ = tokio::task::yield_now() => {}
    }
    assert!(pending.lock().await.contains_key(&1));

    let sender = pending
        .lock()
        .await
        .remove(&1)
        .expect("pending permission sender");
    sender
        .send(serde_json::json!({
            "result": crate::llm::acp_permission::allow_response()
        }))
        .expect("send permission response");

    let response = call.await.expect("permission response");
    assert!(matches!(
        crate::llm::acp_permission::parse_response(&response),
        crate::llm::acp_permission::WireOutcome::Allowed { .. }
    ));
}

async fn wait_for_pending<F>(
    pending: &Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    id: u64,
    mut call: Pin<&mut F>,
) where
    F: Future<Output = Result<serde_json::Value, VmError>>,
{
    for _ in 0..8 {
        tokio::select! {
            result = call.as_mut() => panic!("call completed before entering pending map: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }
        if pending.lock().await.contains_key(&id) {
            return;
        }
    }
    panic!("bridge call {id} did not enter the pending map after bounded polling");
}
