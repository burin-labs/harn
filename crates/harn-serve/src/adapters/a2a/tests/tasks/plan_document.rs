use super::super::{a2a_worker_session_id, A2aWorkerSink};
use super::*;

#[test]
fn a2a_worker_sink_publishes_plan_extension_to_task_stream() {
    let task_id = "task-plan".to_string();
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
    let sink = A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };
    let plan = harn_vm::llm::plan::normalize_plan_tool_call(
        harn_vm::llm::plan::UPDATE_PLAN_TOOL,
        &serde_json::json!({
            "explanation": "Plan the task.",
            "plan": [{"step": "Inspect files.", "status": "pending"}],
        }),
    );

    let event = harn_vm::llm::plan::create_plan_document_event(
        plan,
        "test-agent",
        "test",
        "2026-01-01T00:00:00Z",
        "plan-event-test",
    )
    .expect("plan document");
    sink.handle_event(&harn_vm::agent_events::AgentEvent::PlanDocumentUpdated {
        session_id: a2a_worker_session_id(&task_id),
        event: Box::new(event),
    });

    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    let event = task
        .events
        .iter()
        .find(|event| event.get("type").and_then(JsonValue::as_str) == Some("harn_plan_document"))
        .expect("harn_plan_document event");
    assert_eq!(event["taskId"], task_id);
    assert_eq!(event["entries"][0]["content"], "Inspect files.");
    assert_eq!(
        event["planDocument"]["schema_version"],
        "harn.plan_document.v1"
    );
    assert_eq!(
        event["planDocument"]["current_revision"]["plan"]["schema_version"],
        "harn.plan.v1"
    );
}

#[test]
fn a2a_plan_document_preserves_revision_and_resolution_receipt() {
    use harn_vm::llm::plan::{
        AddPlanComment, ChangePlanCommentState, PlanAuthor, PlanCommentAnchor, PlanCommentState,
        PlanDocumentStore, PlanSource,
    };

    let task_id = "task-plan-receipt".to_string();
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
    let sink = A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };
    let plan = harn_vm::llm::plan::normalize_plan_tool_call(
        harn_vm::llm::plan::UPDATE_PLAN_TOOL,
        &serde_json::json!({"plan": [{"step": "Ship it.", "status": "pending"}]}),
    );
    let created = harn_vm::llm::plan::create_plan_document_event(
        plan,
        "test-agent",
        "test",
        "2026-01-01T00:00:00Z",
        "event-create",
    )
    .expect("create document");
    let mut store = PlanDocumentStore::replay(&[created]).expect("replay create");
    let revision = store.current().current_revision.revision_id.clone();
    store
        .add_comment(AddPlanComment {
            expected_revision_id: revision,
            comment_id: "comment-1".to_string(),
            anchor: PlanCommentAnchor {
                step_id: Some("step-1".to_string()),
                quoted_text: Some("Ship it.".to_string()),
                range: None,
            },
            body: "Prove the release gate.".to_string(),
            author: PlanAuthor {
                id: "reviewer".to_string(),
                display_name: None,
            },
            created_at: "2026-01-01T00:01:00Z".to_string(),
            event_id: "event-comment".to_string(),
        })
        .expect("comment");
    let revision = store.current().current_revision.revision_id.clone();
    store
        .change_comment_state(ChangePlanCommentState {
            expected_revision_id: revision,
            comment_id: "comment-1".to_string(),
            state: PlanCommentState::Resolved,
            author: PlanAuthor {
                id: "agent".to_string(),
                display_name: None,
            },
            source: PlanSource {
                kind: "agent".to_string(),
                uri: None,
            },
            created_at: "2026-01-01T00:02:00Z".to_string(),
            event_id: "event-resolve".to_string(),
            agent_run_id: Some("run-1".to_string()),
            explanation: None,
        })
        .expect("resolve");
    let expected_revision = store.current().current_revision.revision_id.clone();
    sink.handle_event(&harn_vm::agent_events::AgentEvent::PlanDocumentUpdated {
        session_id: a2a_worker_session_id(&task_id),
        event: Box::new(store.events().last().expect("updated event").clone()),
    });

    let tasks = tasks.lock().expect("tasks");
    let event = tasks[&task_id]
        .events
        .iter()
        .find(|event| event.get("type").and_then(JsonValue::as_str) == Some("harn_plan_document"))
        .expect("harn plan document event");
    assert_eq!(
        event["planDocument"]["current_revision"]["revision_id"],
        expected_revision
    );
    assert_eq!(
        event["planDocument"]["resolution_receipts"][0]["output_revision_id"],
        expected_revision
    );
    assert_eq!(
        event["planDocument"]["resolution_receipts"][0]["event_id"],
        "event-resolve"
    );
}
