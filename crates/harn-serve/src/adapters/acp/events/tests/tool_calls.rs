use super::*;

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_serializes_error_category_in_camel_case() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-7".to_string(),
        tool_name: "read".to_string(),
        status: ToolCallStatus::Failed,
        raw_output: None,
        error: Some("missing required arg `path`".to_string()),
        duration_ms: None,
        execution_duration_ms: None,
        error_category: Some(ToolCallErrorCategory::SchemaValidation),
        mutation_status: harn_vm::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        health: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(
        payload["params"]["update"]["sessionUpdate"],
        "tool_call_update"
    );
    assert_eq!(payload["params"]["update"]["status"], "failed");
    assert_eq!(payload["params"]["update"]["name"], "read");
    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["errorCategory"], "schema_validation");
    assert_eq!(harn_meta["error"], "missing required arg `path`");
    assert!(payload["params"]["update"].get("errorCategory").is_none());
    assert!(payload["params"]["update"].get("error").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_omits_error_category_when_none() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-7".to_string(),
        tool_name: "read".to_string(),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({"ok": true})),
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: harn_vm::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        health: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["mutationStatus"], "unknown");
    assert!(harn_meta.get("errorCategory").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_serializes_mutation_status_under_harn_meta() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-7".to_string(),
        tool_name: "edit".to_string(),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({"ok": true})),
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: ToolMutationStatus::NotApplied,
        changed_paths: None,
        data: None,
        health: None,
        executor: Some(ToolExecutor::HostBridge),
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });

    let line = rx.recv().await.expect("acp tool_call_update");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(update_harn_meta(&payload)["mutationStatus"], "not_applied");
    assert!(payload["params"]["update"].get("mutationStatus").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_carries_parsing_flag_through_to_acp_wire() {
    // Harn#692/#904: candidate parser state is Harn metadata on
    // the ACP wire so clients can render the in-flight chip without
    // extending the root ACP tool-call shape.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "session-1".to_string(),
        tool_call_id: "text-cand-1".to_string(),
        tool_name: "edit".to_string(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({}),
        parsing: Some(true),
        audit: None,
        intent: None,
    });
    let line = rx.recv().await.expect("acp tool_call notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(payload["params"]["update"]["sessionUpdate"], "tool_call");
    assert_eq!(update_harn_meta(&payload)["parsing"], true);
    assert_eq!(payload["params"]["update"]["name"], "edit");
    assert!(payload["params"]["update"].get("parsing").is_none());

    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "text-cand-1".to_string(),
        tool_name: "edit".to_string(),
        status: ToolCallStatus::Failed,
        raw_output: None,
        error: Some("malformed args".to_string()),
        duration_ms: None,
        execution_duration_ms: None,
        error_category: Some(ToolCallErrorCategory::ParseAborted),
        mutation_status: harn_vm::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        health: None,
        executor: None,
        parsing: Some(false),
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(
        payload["params"]["update"]["sessionUpdate"],
        "tool_call_update"
    );
    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["parsing"], false);
    assert_eq!(harn_meta["errorCategory"], "parse_aborted");
    assert!(payload["params"]["update"].get("parsing").is_none());
    assert!(payload["params"]["update"].get("errorCategory").is_none());

    // Default `parsing: None` must not surface Harn metadata at all.
    sink.handle_event(&AgentEvent::ToolCall {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-1".to_string(),
        tool_name: "read".to_string(),
        kind: None,
        status: ToolCallStatus::Pending,
        raw_input: serde_json::json!({}),
        parsing: None,
        audit: None,
        intent: None,
    });
    let line = rx.recv().await.expect("acp tool_call notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(payload["params"]["update"].get("_meta").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_serializes_executor_per_acp_wire_format() {
    // Harn#691/#904: clients render badges off Harn executor metadata.
    // The wire shape must distinguish bare-string variants from the
    // McpServer object-with-serverName form so a UI can branch on
    // `typeof executor === "string"`.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

    let cases = [
        (ToolExecutor::HarnBuiltin, serde_json::json!("harn_builtin")),
        (ToolExecutor::HostBridge, serde_json::json!("host_bridge")),
        (
            ToolExecutor::McpServer {
                server_name: "linear".into(),
            },
            serde_json::json!({"kind": "mcp_server", "serverName": "linear"}),
        ),
        (
            ToolExecutor::ProviderNative,
            serde_json::json!("provider_native"),
        ),
    ];

    for (executor, expected) in cases {
        sink.handle_event(&AgentEvent::ToolCallUpdate {
            session_id: "session-1".to_string(),
            tool_call_id: "tool-1".to_string(),
            tool_name: "demo".to_string(),
            status: ToolCallStatus::Completed,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            mutation_status: harn_vm::agent_events::ToolMutationStatus::Unknown,
            changed_paths: None,
            data: None,
            health: None,
            executor: Some(executor),
            parsing: None,
            raw_input: None,
            raw_input_partial: None,
            audit: None,
        });
        let line = rx.recv().await.expect("acp tool_call_update notification");
        let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(
            payload["params"]["update"]["sessionUpdate"],
            "tool_call_update"
        );
        assert_eq!(update_harn_meta(&payload)["executor"], expected);
        assert!(payload["params"]["update"].get("executor").is_none());
    }

    // `executor: None` must not surface executor metadata. The typed
    // mutation outcome is still mandatory for every tool_call_update.
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-2".to_string(),
        tool_name: "demo".to_string(),
        status: ToolCallStatus::InProgress,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: harn_vm::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        health: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["mutationStatus"], "unknown");
    assert!(harn_meta.get("executor").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn tool_call_update_streams_raw_input_and_raw_input_partial_per_acp_wire_format() {
    // #693/#904: parsed raw input remains canonical `rawInput`;
    // unparseable raw bytes are Harn metadata under `_meta.harn`.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let sink = AcpAgentEventSink::new(AcpOutput::Channel(tx));

    // Parsed partial value → `rawInput` populated, `rawInputPartial` absent.
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-streaming".to_string(),
        tool_name: "search".to_string(),
        status: ToolCallStatus::Pending,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: harn_vm::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        health: None,
        executor: None,
        raw_input: Some(serde_json::json!({"q": "hello"})),
        raw_input_partial: None,
        audit: None,
        parsing: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(payload["params"]["update"]["rawInput"]["q"], "hello");
    let harn_meta = update_harn_meta(&payload);
    assert_eq!(harn_meta["mutationStatus"], "unknown");
    assert!(harn_meta.get("rawInputPartial").is_none());

    // Unparseable partial bytes → `rawInputPartial` populated, `rawInput` absent.
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-streaming".to_string(),
        tool_name: "search".to_string(),
        status: ToolCallStatus::Pending,
        raw_output: None,
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        mutation_status: harn_vm::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        health: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: Some(r#"{"q":"hel"#.to_string()),
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(payload["params"]["update"].get("rawInput").is_none());
    assert_eq!(
        update_harn_meta(&payload)["rawInputPartial"],
        r#"{"q":"hel"#
    );
    assert!(payload["params"]["update"].get("rawInputPartial").is_none());

    // Terminal updates (None / None) must not introduce these keys.
    sink.handle_event(&AgentEvent::ToolCallUpdate {
        session_id: "session-1".to_string(),
        tool_call_id: "tool-streaming".to_string(),
        tool_name: "search".to_string(),
        status: ToolCallStatus::Completed,
        raw_output: Some(serde_json::json!({"ok": true})),
        error: None,
        duration_ms: Some(12),
        execution_duration_ms: Some(8),
        error_category: None,
        mutation_status: harn_vm::agent_events::ToolMutationStatus::Unknown,
        changed_paths: None,
        data: None,
        health: None,
        executor: None,
        parsing: None,
        raw_input: None,
        raw_input_partial: None,
        audit: None,
    });
    let line = rx.recv().await.expect("acp tool_call_update notification");
    let payload: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert!(payload["params"]["update"].get("rawInput").is_none());
    assert!(update_harn_meta(&payload).get("rawInputPartial").is_none());
    assert_eq!(payload["params"]["update"]["status"], "completed");
}
