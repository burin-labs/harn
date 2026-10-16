use harn_session_store::task::{
    AdmitTask, NewTaskControlEvent, StoredTaskRetry, TaskAdmission, TaskRecord,
    TaskSessionDescriptor, TaskStatus, TaskStore, TaskUpdate, TASK_RETRY_RETENTION_MS,
};
use harn_session_store::{
    EventRedactor, MemorySessionStore, SessionStore, SqliteSessionStore, StoreContention,
    StoreError, StoreHooks,
};
use serde_json::json;
use std::sync::{Arc, Barrier};

pub(crate) fn request(id: &str, fingerprint: &str, now_ms: i64) -> AdmitTask {
    AdmitTask {
        task: TaskRecord {
            id: id.into(),
            session_id: "session-one".into(),
            workspace_id: "workspace-one".into(),
            created_at: "2026-10-11T00:00:00Z".into(),
            updated_at: "2026-10-11T00:00:00Z".into(),
            status: TaskStatus::Working,
            input: json!({"text":"hello"}),
            metadata: json!({}),
            created_by: "api".into(),
            persona_id: None,
            branch_id: None,
            parent_task_id: None,
            assigned_agent_id: None,
            receipt_id: None,
            outcome_id: None,
            quota_id: None,
            started_at: Some("2026-10-11T00:00:00Z".into()),
            completed_at: None,
            canceled_at: None,
            failure: None,
        },
        session: TaskSessionDescriptor {
            id: "session-one".into(),
            workspace_id: "workspace-one".into(),
            workspace_root: "/workspace/one".into(),
            mode_id: "ask".into(),
            model_policy: None,
            metadata: json!({}),
            persona_id: None,
            root_session_id: None,
            parent_session_id: None,
            branch_id: None,
            created_at: "2026-10-11T00:00:00Z".into(),
            updated_at: "2026-10-11T00:00:00Z".into(),
            summary: None,
            status: harn_session_store::SessionStatus::Open,
        },
        retry: Some(StoredTaskRetry {
            binding_digest: "a".repeat(64),
            request_fingerprint: fingerprint.repeat(64),
        }),
        initial_events: vec![NewTaskControlEvent {
            event: "message.created".into(),
            payload: json!({"text":"hello"}),
        }],
        now_ms,
    }
}

fn admission_contract(store: &dyn TaskStore) {
    assert!(store.list_task_sessions().unwrap().is_empty());
    assert!(matches!(
        store.admit_task(request("first", "b", 0)).unwrap(),
        TaskAdmission::Created(_)
    ));
    let claim = store.claim_task_execution("first").unwrap();
    assert!(matches!(
        store.claim_task_execution("first"),
        Err(StoreError::Contention {
            kind: StoreContention::TaskExecutionHeld,
            ..
        })
    ));
    assert!(store.claim_task_execution("other").is_ok());
    assert!(
        matches!(store.admit_task(request("retry", "b", TASK_RETRY_RETENTION_MS + 1)).unwrap(), TaskAdmission::Reused(task) if task.id == "first")
    );
    assert!(matches!(
        store.admit_task(request("conflict", "c", 1)).unwrap(),
        TaskAdmission::Conflict
    ));
    assert_eq!(store.list_tasks(None, None).unwrap().len(), 1);
    assert_eq!(
        store.list_task_sessions().unwrap(),
        [store.get_task_session("session-one").unwrap()]
    );
    assert_eq!(store.task_events(None, None).unwrap().len(), 2);
    let completed = store
        .update_task(
            "first",
            TaskUpdate {
                status: TaskStatus::Completed,
                at: "completed".into(),
                failure: None,
            },
        )
        .unwrap();
    assert!(completed.event.is_some());
    assert!(store
        .update_task(
            "first",
            TaskUpdate {
                status: TaskStatus::Canceled,
                at: "later".into(),
                failure: None
            }
        )
        .unwrap()
        .event
        .is_none());
    assert_eq!(
        store.get_task("first").unwrap().status,
        TaskStatus::Completed
    );
    assert!(
        matches!(store.admit_task(request("before-expiry", "b", TASK_RETRY_RETENTION_MS - 1)).unwrap(), TaskAdmission::Reused(task) if task.id == "first")
    );
    // The late active retry did not slide the original expiry.
    assert!(
        matches!(store.admit_task(request("after-expiry", "c", TASK_RETRY_RETENTION_MS)).unwrap(), TaskAdmission::Created(task) if task.id == "after-expiry")
    );
    drop(claim);
    assert_eq!(
        store.list_task_sessions().unwrap().len(),
        1,
        "multiple tasks share one descriptor"
    );
    assert!(store.claim_task_execution("first").is_ok());
}

#[test]
fn task_admission_retention_and_terminal_contract_matches_backends() {
    admission_contract(&MemorySessionStore::new());
    admission_contract(&SqliteSessionStore::open_in_memory().unwrap());
}

fn lifecycle_and_input_contract(store: &dyn TaskStore) {
    let mut input = request("lifecycle", "b", 0);
    input.task.status = TaskStatus::Submitted;
    input.task.started_at = None;
    store.admit_task(input).unwrap();
    assert_eq!(
        store.task_events(None, None).unwrap()[0].event,
        "task.submitted"
    );
    assert_eq!(store.get_task("lifecycle").unwrap().started_at, None);
    let update = |status, at: &str| TaskUpdate {
        status,
        at: at.into(),
        failure: None,
    };
    let started = store
        .update_task("lifecycle", update(TaskStatus::Working, "started"))
        .unwrap();
    assert_eq!(started.previous_status, TaskStatus::Submitted);
    assert_eq!(started.event.unwrap().event, "task.started");
    assert_eq!(started.task.started_at.as_deref(), Some("started"));
    let waiting = store
        .update_task("lifecycle", update(TaskStatus::AuthRequired, "approval"))
        .unwrap();
    assert_eq!(waiting.event.unwrap().event, "task.auth_required");
    let resumed = store
        .update_task("lifecycle", update(TaskStatus::Working, "resumed"))
        .unwrap();
    assert_eq!(resumed.event.unwrap().event, "task.resumed");
    assert_eq!(resumed.task.started_at.as_deref(), Some("started"));
    assert!(store
        .append_task_input("lifecycle", "wrong-session", json!({"id":"wrong"}))
        .is_err());
    let input = store
        .append_task_input("lifecycle", "session-one", json!({"id":"accepted"}))
        .unwrap();
    assert_eq!(input.event, "message.created");
    assert_eq!(input.payload["id"], "accepted");
    store
        .update_task("lifecycle", update(TaskStatus::Canceled, "canceled"))
        .unwrap();
    let before = store.task_events(None, None).unwrap();
    assert!(store
        .append_task_input("lifecycle", "session-one", json!({"id":"late"}))
        .is_err());
    assert!(store
        .update_task("lifecycle", update(TaskStatus::Working, "late"))
        .unwrap()
        .event
        .is_none());
    assert_eq!(store.task_events(None, None).unwrap(), before);
}

#[test]
fn task_lifecycle_and_input_admission_match_backends() {
    lifecycle_and_input_contract(&MemorySessionStore::new());
    lifecycle_and_input_contract(&SqliteSessionStore::open_in_memory().unwrap());
}

#[test]
fn sqlite_submitted_and_auth_required_states_survive_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sessions.sqlite");
    let store = SqliteSessionStore::open(&path).unwrap();
    let mut input = request("submitted", "b", 0);
    input.task.status = TaskStatus::Submitted;
    input.task.started_at = None;
    store.admit_task(input).unwrap();
    drop(store);
    let store = SqliteSessionStore::open(&path).unwrap();
    assert_eq!(
        store.get_task("submitted").unwrap().status,
        TaskStatus::Submitted
    );
    for status in [TaskStatus::Working, TaskStatus::AuthRequired] {
        store
            .update_task(
                "submitted",
                TaskUpdate {
                    status,
                    at: "approval".into(),
                    failure: None,
                },
            )
            .unwrap();
    }
    drop(store);
    let store = SqliteSessionStore::open(&path).unwrap();
    assert_eq!(
        store.get_task("submitted").unwrap().status,
        TaskStatus::AuthRequired
    );
    assert!(
        matches!(store.admit_task(request("retry", "b", TASK_RETRY_RETENTION_MS + 1)).unwrap(), TaskAdmission::Reused(task) if task.id == "submitted")
    );
}

#[tokio::test]
async fn sqlite_task_retry_and_numeric_control_cursor_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.sqlite");
    let store = SqliteSessionStore::open(&path).unwrap();
    assert_eq!(
        store.next_control_event_id().unwrap(),
        "event_0000000000000001"
    );
    store.admit_task(request("first", "b", 0)).unwrap();
    let before = store.task_events(None, None).unwrap();
    assert_eq!(
        before
            .iter()
            .map(|event| event.id.as_str())
            .collect::<Vec<_>>(),
        ["event_0000000000000002", "event_0000000000000003"]
    );
    assert_eq!(
        store.next_control_event_id().unwrap(),
        "event_0000000000000004"
    );
    drop(store);
    let reopened = SqliteSessionStore::open(&path).unwrap();
    assert_eq!(
        reopened.list_task_sessions().unwrap(),
        [reopened.get_task_session("session-one").unwrap()]
    );
    assert!(
        matches!(reopened.admit_task(request("retry", "b", 1)).unwrap(), TaskAdmission::Reused(task) if task.id == "first")
    );
    assert_eq!(reopened.task_events(None, None).unwrap(), before);
    assert_eq!(
        reopened
            .get_task_session("session-one")
            .unwrap()
            .workspace_root,
        "/workspace/one"
    );
    let event = reopened
        .append_task_event(
            "first",
            NewTaskControlEvent {
                event: "message.created".into(),
                payload: json!({}),
            },
        )
        .unwrap();
    assert_eq!(event.id, "event_0000000000000005");
    let cursor = u64::from_str_radix(event.id.strip_prefix("event_").unwrap(), 16).unwrap();
    assert_eq!(cursor, 5);
    assert!(before.iter().all(|previous| previous.id < event.id));
    assert!(
        matches!(
            reopened.describe("session-one").await,
            Err(StoreError::NotFound(_))
        ),
        "task descriptors must not create a second transcript session"
    );
}

#[test]
fn sqlite_concurrent_task_retry_has_one_admission_and_initial_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.sqlite");
    let first = SqliteSessionStore::open(&path).unwrap();
    let second = SqliteSessionStore::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let other_barrier = barrier.clone();
    let other = std::thread::spawn(move || {
        other_barrier.wait();
        second.admit_task(request("second", "b", 0)).unwrap()
    });
    barrier.wait();
    let results = [
        first.admit_task(request("first", "b", 0)).unwrap(),
        other.join().unwrap(),
    ];
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, TaskAdmission::Created(_)))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, TaskAdmission::Reused(_)))
            .count(),
        1
    );
    assert_eq!(first.list_tasks(None, None).unwrap().len(), 1);
    assert_eq!(first.task_events(None, None).unwrap().len(), 2);
}

struct CanaryRedactor;

fn message_projection_contract(store: &dyn TaskStore) {
    let admit = |id: &str| {
        let mut request = request(id, "b", 0);
        request.retry = None;
        store.admit_task(request).unwrap();
    };
    admit("first");
    let stale = store.task_message_view("session-one").unwrap();
    assert_eq!(stale.events.len(), 1);
    admit("second");
    let current = store.task_message_view("session-one").unwrap();
    assert_eq!(current.events.len(), 2);
    store
        .retain_task_messages(
            "session-one",
            current.through_event_id.as_deref(),
            &[current.events[0].id.clone()],
        )
        .unwrap();
    assert_eq!(
        store.task_message_view("session-one").unwrap().events.len(),
        1
    );
    // An older snapshot cannot move the watermark backwards and reveal second.
    store
        .retain_task_messages("session-one", stale.through_event_id.as_deref(), &[])
        .unwrap();
    assert!(store
        .task_message_view("session-one")
        .unwrap()
        .events
        .is_empty());
    admit("third");
    // Neither an explicitly retained hidden ID nor an old snapshot resurrects
    // old messages. The newly committed message was outside that snapshot.
    store
        .retain_task_messages(
            "session-one",
            current.through_event_id.as_deref(),
            &[current.events[1].id.clone()],
        )
        .unwrap();
    let newest = store.task_message_view("session-one").unwrap();
    assert_eq!(newest.events.len(), 1);
    assert_eq!(newest.events[0].task_id, "third");
    store
        .retain_task_messages("session-one", newest.through_event_id.as_deref(), &[])
        .unwrap();
    assert!(store
        .task_message_view("session-one")
        .unwrap()
        .events
        .is_empty());
    assert_eq!(
        store.task_events(None, None).unwrap().len(),
        6,
        "audit records remain intact"
    );
}

#[test]
fn task_message_truncation_preserves_new_messages_and_rejects_stale_resurrection() {
    message_projection_contract(&MemorySessionStore::new());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.sqlite");
    let store = SqliteSessionStore::open(&path).unwrap();
    message_projection_contract(&store);
    drop(store);
    let reopened = SqliteSessionStore::open(&path).unwrap();
    assert!(reopened
        .task_message_view("session-one")
        .unwrap()
        .events
        .is_empty());
    assert_eq!(reopened.task_events(None, None).unwrap().len(), 6);
}

impl EventRedactor for CanaryRedactor {
    fn redact_json_in_place(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) if text.contains("task-secret-canary") => {
                *text = "[redacted]".into();
            }
            serde_json::Value::Object(object) => {
                for value in object.values_mut() {
                    self.redact_json_in_place(value);
                }
            }
            serde_json::Value::Array(array) => {
                for value in array {
                    self.redact_json_in_place(value);
                }
            }
            _ => {}
        }
    }

    fn redact_headers(
        &self,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> std::collections::BTreeMap<String, String> {
        headers.clone()
    }
}

fn redaction_contract(store: &dyn TaskStore) {
    let mut input = request("redacted", "b", 0);
    input.task.input = json!({"text":"task-secret-canary"});
    input.task.metadata = json!({"token":"task-secret-canary"});
    input.session.metadata = json!({"token":"task-secret-canary"});
    input.session.model_policy = Some(json!({"provider":"mock","model":"task-secret-canary"}));
    input.initial_events[0].payload = json!({"text":"task-secret-canary"});
    store.admit_task(input).unwrap();
    store
        .update_task(
            "redacted",
            TaskUpdate {
                status: TaskStatus::Failed,
                at: "failed".into(),
                failure: Some(harn_session_store::task::TaskFailure {
                    code: "failed".into(),
                    message: "task-secret-canary".into(),
                }),
            },
        )
        .unwrap();
    assert_eq!(
        store.get_task("redacted").unwrap().input["text"],
        "[redacted]"
    );
    assert_eq!(
        store.get_task_session("session-one").unwrap().metadata["token"],
        "[redacted]"
    );
    assert_eq!(
        store
            .get_task_session("session-one")
            .unwrap()
            .model_policy
            .unwrap()["model"],
        "[redacted]"
    );
    assert!(!serde_json::to_string(&store.get_task("redacted").unwrap())
        .unwrap()
        .contains("task-secret-canary"));
    assert!(
        !serde_json::to_string(&store.task_events(None, None).unwrap())
            .unwrap()
            .contains("task-secret-canary")
    );
}

#[test]
fn task_admission_redacts_payloads_before_storage_and_restart_readback() {
    let hooks = StoreHooks {
        redaction: Some(Arc::new(CanaryRedactor)),
        ..StoreHooks::default()
    };
    redaction_contract(&MemorySessionStore::with_hooks(hooks.clone()));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sessions.sqlite");
    let store = SqliteSessionStore::open_with_hooks(&path, hooks).unwrap();
    redaction_contract(&store);
    drop(store);
    // Read without a redactor to prove the persisted values were already safe.
    let reopened = SqliteSessionStore::open(&path).unwrap();
    assert_eq!(
        reopened
            .get_task_session("session-one")
            .unwrap()
            .model_policy
            .unwrap()["model"],
        "[redacted]"
    );
    assert_eq!(
        reopened
            .get_task("redacted")
            .unwrap()
            .failure
            .unwrap()
            .message,
        "[redacted]"
    );
    assert!(
        !serde_json::to_string(&reopened.task_events(None, None).unwrap())
            .unwrap()
            .contains("task-secret-canary")
    );
}
