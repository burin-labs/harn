use crate::NativeBuiltinCtxExt;
use std::sync::Arc;

use super::host_agent_dispatch_tool_call;

#[tokio::test(flavor = "current_thread")]
async fn explicit_hook_truncation_emits_even_when_final_length_is_unchanged() {
    crate::orchestration::clear_tool_hooks();
    let captured = crate::boundary::tests::CapturedEvents::install();
    crate::orchestration::register_tool_hook(crate::orchestration::ToolHook {
        pattern: "read_file".to_string(),
        pre: None,
        post: Some(Arc::new(|_name, result: &str| {
            #[expect(clippy::string_slice, reason = "test input is ASCII")]
            let kept = &result[..result.len() - 4];
            crate::orchestration::PostToolAction::Truncate {
                result: format!("{kept}TAIL"),
                dropped_bytes: 4,
            }
        })),
    });

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("dispatch-proof.txt");
    std::fs::write(&path, "same length proof\n").expect("write fixture");
    let tools = crate::schema::json_to_vm_value(&serde_json::json!({
        "tools": [{
            "name": "read_file",
            "description": "Read a file through the local Harn executor.",
            "parameters": {"path": {"type": "string"}}
        }]
    }));
    let call = crate::schema::json_to_vm_value(&serde_json::json!({
        "id": "truncation-dispatch",
        "name": "read_file",
        "arguments": {"path": path}
    }));

    let result = host_agent_dispatch_tool_call(
        crate::vm::AsyncBuiltinCtx::for_test(crate::new_vm()),
        call,
        Some(&tools),
        &crate::value::DictMap::new(),
    )
    .await
    .expect("dispatch succeeds");
    crate::orchestration::clear_tool_hooks();
    let result = crate::llm::helpers::vm_value_to_json(&result);

    assert_eq!(result["tool_output_truncated"], serde_json::json!(true));
    assert_eq!(result["dropped_bytes"], serde_json::json!(4));
    assert_eq!(
        result["original_size"], result["final_size"],
        "the typed hook fact, not a net-length heuristic, must drive the event"
    );
    let events = captured.boundary_failures();
    assert_eq!(events.len(), 1, "got: {events:?}");
    match &events[0] {
        crate::agent_events::AgentEvent::BoundaryFailure {
            boundary,
            kind,
            dropped_bytes,
            ..
        } => {
            assert_eq!(*boundary, crate::boundary::BoundaryId::PostToolOutput);
            assert_eq!(boundary.as_str(), "post_tool_output");
            assert_eq!(*kind, crate::boundary::BoundaryFailureKind::Truncated);
            assert_eq!(*dropped_bytes, 4);
        }
        other => panic!("expected boundary failure, got {other:?}"),
    }
}

fn read_file_tools() -> crate::value::VmValue {
    crate::schema::json_to_vm_value(&serde_json::json!({
        "tools": [{
            "name": "read_file",
            "description": "Read a file through the local Harn executor.",
            "parameters": {"path": {"type": "string"}}
        }]
    }))
}

async fn dispatch_read_file(
    path: &std::path::Path,
    call_id: &str,
    options: serde_json::Value,
) -> serde_json::Value {
    let call = crate::schema::json_to_vm_value(&serde_json::json!({
        "id": call_id,
        "name": "read_file",
        "arguments": {"path": path}
    }));
    let options = match crate::schema::json_to_vm_value(&options) {
        crate::value::VmValue::Dict(options) => (*options).clone(),
        _ => crate::value::DictMap::new(),
    };
    let result = host_agent_dispatch_tool_call(
        crate::vm::AsyncBuiltinCtx::for_test(crate::new_vm()),
        call,
        Some(&read_file_tools()),
        &options,
    )
    .await
    .expect("dispatch succeeds");
    crate::llm::helpers::vm_value_to_json(&result)
}

/// The next provider request body after recording `dispatch` as the answer to
/// one native read_file call.
fn next_anthropic_request(session_id: &str, call_id: &str, dispatch: &serde_json::Value) -> String {
    crate::llm::agent_session_host::seed_host_session_provider_model(
        session_id,
        "anthropic",
        "claude-opus-4-8",
    );
    for message in [
        serde_json::json!({"role": "user", "content": "read the file"}),
        serde_json::json!({
            "role": "assistant",
            "content": [{"type": "tool_use", "id": call_id, "name": "read_file", "input": {}}],
        }),
    ] {
        crate::agent_sessions::inject_message(
            session_id,
            crate::schema::json_to_vm_value(&message),
        )
        .expect("inject message");
    }
    crate::llm::agent_session_host::record_tool_results_for_test(
        session_id,
        crate::schema::json_to_vm_value(&serde_json::json!([dispatch])),
    );
    let transcript = crate::agent_sessions::transcript(session_id).expect("transcript");
    let messages: Vec<crate::value::VmValue> =
        match transcript.as_dict().and_then(|dict| dict.get("messages")) {
            Some(crate::value::VmValue::List(list)) => list.iter().cloned().collect(),
            _ => Vec::new(),
        };
    let opts = crate::llm::api::LlmCallOptions {
        provider: "anthropic".to_string(),
        model: "claude-opus-4-8".to_string(),
        messages: crate::llm::helpers::vm_messages_to_json(&messages).expect("messages json"),
        ..Default::default()
    };
    crate::llm::providers::AnthropicProvider::build_request_body(
        &crate::llm::api::LlmRequestPayload::from(&opts),
    )
    .to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn oversized_result_spills_and_the_next_request_carries_a_bounded_excerpt() {
    crate::orchestration::clear_tool_hooks();
    crate::llm::agent_session_host::reset_agent_session_host_state();
    let captured = crate::boundary::tests::CapturedEvents::install();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("codemod-dry-run.txt");
    let mut source = String::from("HEAD-MARKER\n");
    for _ in 0..1998 {
        source.push_str(&"x".repeat(1_100));
        source.push('\n');
    }
    source.push_str("TAIL-MARKER\n");
    std::fs::write(&path, &source).expect("write fixture");

    // The unbudgeted rendering is the full text the spill file must hold.
    let full = dispatch_read_file(
        &path,
        "budget-off",
        serde_json::json!({"tool_result_max_chars": 0}),
    )
    .await;
    let full_text = full["rendered_result"]
        .as_str()
        .expect("rendered")
        .to_string();
    assert!(
        full_text.len() > 2_000_000,
        "fixture renders {} bytes",
        full_text.len()
    );
    assert_eq!(full["tool_output_truncated"], serde_json::json!(false));

    let session_id =
        crate::agent_sessions::open_or_create_for_test(Some("tool-result-budget".to_string()));
    let result = dispatch_read_file(
        &path,
        "budget-on",
        serde_json::json!({"session_id": &session_id}),
    )
    .await;
    crate::orchestration::clear_tool_hooks();

    let rendered = result["rendered_result"].as_str().expect("rendered");
    assert!(
        rendered.len() <= 64_000,
        "rendered {} bytes",
        rendered.len()
    );
    assert!(rendered.contains("HEAD-MARKER") && rendered.contains("TAIL-MARKER"));
    assert_eq!(result["tool_output_truncated"], serde_json::json!(true));
    assert_eq!(result["original_size"], serde_json::json!(full_text.len()));
    assert_eq!(result["final_size"], serde_json::json!(rendered.len()));
    assert!(result["dropped_bytes"].as_u64().unwrap_or(0) > 1_900_000);
    let spill = std::path::PathBuf::from(result["full_output_path"].as_str().expect("spill path"));
    assert!(rendered.contains(&format!("full output saved to {}]", spill.display())));
    let spill_root = crate::stdlib::fs::workspace_temp_root()
        .expect("workspace temp root")
        .join("tool-results");
    assert!(
        spill.starts_with(&spill_root),
        "{spill:?} under {spill_root:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&spill).expect("spill file"),
        full_text
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&spill)
            .expect("spill metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let events = captured.boundary_failures();
    assert!(
        events.iter().any(|event| matches!(
            event,
            crate::agent_events::AgentEvent::BoundaryFailure {
                boundary: crate::boundary::BoundaryId::PostToolOutput,
                kind: crate::boundary::BoundaryFailureKind::Capped,
                ..
            }
        )),
        "got: {events:?}"
    );

    let body = next_anthropic_request(&session_id, "budget-on", &result);
    assert!(
        body.len() < 80_000,
        "next request carries {} bytes",
        body.len()
    );
    assert!(body.contains("full output saved to"));

    assert!(crate::agent_sessions::close(&session_id));
    assert!(
        !spill.exists(),
        "closing the session removes its spill file"
    );
    assert!(!spill.parent().expect("session dir").exists());
}

#[tokio::test(flavor = "current_thread")]
async fn a_result_under_budget_reaches_the_model_unchanged() {
    crate::orchestration::clear_tool_hooks();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("small.txt");
    std::fs::write(&path, "small result\n").expect("write fixture");
    let result = dispatch_read_file(&path, "budget-small", serde_json::json!({})).await;
    assert_eq!(result["tool_output_truncated"], serde_json::json!(false));
    assert_eq!(result["full_output_path"], serde_json::Value::Null);
    assert_eq!(result["original_size"], result["final_size"]);
    assert!(!result["observation"]
        .as_str()
        .expect("observation")
        .contains("tool output truncated"));
}
