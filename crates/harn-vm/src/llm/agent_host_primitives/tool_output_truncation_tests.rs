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
    let tools = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "tools": [{
            "name": "read_file",
            "description": "Read a file through the local Harn executor.",
            "parameters": {"path": {"type": "string"}}
        }]
    }));
    let call = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "id": "truncation-dispatch",
        "name": "read_file",
        "arguments": {"path": path}
    }));

    let result = host_agent_dispatch_tool_call(
        crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
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
