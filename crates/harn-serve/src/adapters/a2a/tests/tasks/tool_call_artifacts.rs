use super::*;

#[test]
fn noncompleted_tool_calls_do_not_emit_artifact_updates() {
    let task_id = "task-tool-pending".to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id: None,
        status: TaskStatus::Working,
        history: Vec::new(),
        artifacts: Vec::new(),
        metadata: BTreeMap::new(),
        events: Vec::new(),
        subscribers: Vec::new(),
        cancel_token: None,
    };
    let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
    let sink = super::super::A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };

    for event in [
        harn_vm::agent_events::AgentEvent::ToolCallUpdate {
            session_id: super::super::a2a_worker_session_id(&task_id),
            tool_call_id: "tc-99".into(),
            tool_name: "search_files".into(),
            status: harn_vm::agent_events::ToolCallStatus::InProgress,
            raw_output: None,
            error: None,
            duration_ms: None,
            execution_duration_ms: None,
            error_category: None,
            mutation_status: harn_vm::agent_events::ToolMutationStatus::Unknown,
            changed_paths: None,
            data: None,
            executor: None,
            parsing: None,
            raw_input: None,
            raw_input_partial: None,
            audit: None,
        },
        harn_vm::agent_events::AgentEvent::ToolCallUpdate {
            session_id: super::super::a2a_worker_session_id(&task_id),
            tool_call_id: "tc-100".into(),
            tool_name: "search_files".into(),
            status: harn_vm::agent_events::ToolCallStatus::Failed,
            raw_output: None,
            error: Some("boom".into()),
            duration_ms: Some(1),
            execution_duration_ms: Some(1),
            error_category: None,
            mutation_status: harn_vm::agent_events::ToolMutationStatus::Unknown,
            changed_paths: None,
            data: None,
            executor: None,
            parsing: None,
            raw_input: None,
            raw_input_partial: None,
            audit: None,
        },
    ] {
        sink.handle_event(&event);
    }

    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    assert!(task.artifacts.is_empty());
    assert!(!task
        .events
        .iter()
        .any(|event| event.get("kind").and_then(JsonValue::as_str) == Some("artifact-update")));
}
