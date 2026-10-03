use super::super::{a2a_worker_session_id, A2aWorkerSink};
use super::*;
use harn_vm::agent_events::session_health::{
    session_health_schema, HealthHeuristics, HealthMeasurements, SessionHealthFact,
    SESSION_HEALTH_SCHEMA_VERSION,
};

#[test]
fn session_health_uses_the_checked_task_event_without_changing_task_state() {
    let task_id = "task-health".to_owned();
    let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(
        task_id.clone(),
        TaskState {
            id: task_id.clone(),
            context_id: Some("context-health".to_owned()),
            status: TaskStatus::InputRequired,
            history: Vec::new(),
            artifacts: Vec::new(),
            metadata: BTreeMap::new(),
            events: Vec::new(),
            subscribers: Vec::new(),
            cancel_token: None,
        },
    )])));
    let fact = SessionHealthFact {
        schema_version: SESSION_HEALTH_SCHEMA_VERSION,
        session_id: a2a_worker_session_id(&task_id),
        iteration: Some(1),
        turn: HealthMeasurements::default(),
        rolling: HealthMeasurements::default(),
        heuristics: HealthHeuristics::default(),
    };
    let sink = A2aWorkerSink {
        task_id: task_id.clone(),
        tasks: tasks.clone(),
    };
    sink.handle_event(&harn_vm::agent_events::AgentEvent::SessionHealth {
        session_id: fact.session_id.clone(),
        fact: Box::new(fact.clone()),
    });
    let tasks = tasks.lock().expect("tasks");
    let task = tasks.get(&task_id).expect("task");
    assert_eq!(task.status, TaskStatus::InputRequired);
    assert_eq!(task.events.len(), 1);
    let event = &task.events[0];
    assert_eq!(event["contextId"], "context-health");
    assert_eq!(event["status"]["state"], "input-required");
    assert_eq!(event["final"], false);

    let schema: JsonValue = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/protocols/schemas/a2a-0.3.0.schema.json"
    )))
    .expect("owning A2A schema");
    let validator = jsonschema::validator_for(&schema).expect("A2A stream validator");
    assert!(
        !validator.is_valid(&super::super::wrap_event(
            JsonValue::Null,
            json!({
                "type": "harn_session_health", "taskId": task_id, "health": fact,
            })
        )),
        "the old unregistered envelope must be refused"
    );
    validator
        .validate(&super::super::wrap_event(JsonValue::Null, event.clone()))
        .expect("actual task stream event");
    let payload = &event["metadata"]["harn"]["sessionHealth"];
    jsonschema::validator_for(&session_health_schema())
        .expect("runtime-owned health schema")
        .validate(payload)
        .expect("unchanged measured fact");
    assert_eq!(payload, &serde_json::to_value(fact).expect("health fact"));
}
