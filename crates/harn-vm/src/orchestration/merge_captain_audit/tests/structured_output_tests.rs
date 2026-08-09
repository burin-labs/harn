use super::*;

#[test]
fn flags_invalid_structured_output_from_failed_tool_update() {
    let events = vec![
        iteration_start(1, "s", 1),
        tool_call(2, "s", "list_checks", json!({"a": 1})),
        env(
            3,
            AgentEvent::ToolCallUpdate {
                session_id: "s".into(),
                tool_call_id: "call_2".into(),
                tool_name: "list_checks".into(),
                status: ToolCallStatus::Failed,
                raw_output: None,
                error: Some("missing required field".into()),
                duration_ms: None,
                execution_duration_ms: None,
                error_category: Some(ToolCallErrorCategory::SchemaValidation),
                mutation_status: crate::agent_events::ToolMutationStatus::Unknown,
                changed_paths: None,
                data: None,
                executor: None,
                parsing: None,
                raw_input: None,
                raw_input_partial: None,
                audit: None,
            },
        ),
        iteration_end(4, "s", 1),
    ];
    let report = audit_transcript(&events, None);
    assert!(report
        .findings
        .iter()
        .any(|finding| finding.category == FindingCategory::InvalidStructuredOutput));
}
