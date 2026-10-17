use super::task::request;
use harn_session_store::task::{TaskFailure, TaskSessionUpdate, TaskStatus, TaskStore, TaskUpdate};
use harn_session_store::{
    EventRedactor, MemorySessionStore, SessionStatus, SqliteSessionStore, StoreError, StoreHooks,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

fn mutation_contract(store: &dyn TaskStore) {
    let mut first = request("first", "b", 0);
    first.task.status = TaskStatus::Submitted;
    first.session.root_session_id = Some("root".into());
    first.session.parent_session_id = Some("parent".into());
    first.session.branch_id = Some("branch".into());
    store.admit_task(first).unwrap();
    let change = store
        .update_task_session(
            "session-one",
            TaskSessionUpdate {
                metadata: Some(json!({"changed":true})),
                summary: Some(Some("summary".into())),
                model_policy: Some(Some(json!({"provider":"mock", "model":"o3"}))),
                updated_at: "2026-10-12T00:00:00Z".into(),
                ..TaskSessionUpdate::default()
            },
        )
        .unwrap();
    assert!(change.events.is_empty());
    assert_eq!(
        store.get_task_session("session-one").unwrap(),
        change.session
    );
    let mut second = request("second", "c", 1);
    second.task.status = TaskStatus::Submitted;
    second.retry = None;
    store.admit_task(second).unwrap();
    assert_eq!(
        store.get_task_session("session-one").unwrap(),
        change.session,
        "another task cannot overwrite updated session facts with its admission snapshot"
    );
    let change = store
        .update_task_session(
            "session-one",
            TaskSessionUpdate {
                status: Some(SessionStatus::Closed),
                updated_at: "2026-10-12T00:01:00Z".into(),
                ..TaskSessionUpdate::default()
            },
        )
        .unwrap();
    assert_eq!(change.events.len(), 2);
    assert!(change
        .events
        .iter()
        .all(|event| event.event == "task.canceled"));
    for id in ["first", "second"] {
        assert_eq!(store.get_task(id).unwrap().status, TaskStatus::Canceled);
        assert_eq!(
            store
                .update_task(
                    id,
                    TaskUpdate {
                        status: TaskStatus::Working,
                        at: "later".into(),
                        failure: None
                    }
                )
                .unwrap()
                .task
                .status,
            TaskStatus::Canceled
        );
    }
    let mut new = request("must-not-admit", "c", 2);
    new.retry = None;
    assert!(matches!(
        store.admit_task(new),
        Err(StoreError::Conflict(_))
    ));
    assert_eq!(store.list_tasks(None, None).unwrap().len(), 2);
    assert_eq!(
        store.get_task_session("session-one").unwrap().status,
        SessionStatus::Closed
    );
}

#[test]
fn updates_preserve_facts_and_close_all_admitted_tasks() {
    mutation_contract(&MemorySessionStore::new());
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("store.sqlite");
    mutation_contract(&SqliteSessionStore::open(&path).unwrap());
    let reopened = SqliteSessionStore::open(&path).unwrap();
    let descriptor = reopened.get_task_session("session-one").unwrap();
    assert_eq!(descriptor.metadata, json!({"changed":true}));
    assert_eq!(descriptor.summary.as_deref(), Some("summary"));
    assert_eq!(descriptor.model_policy.unwrap()["model"], "o3");
    assert_eq!(descriptor.status, SessionStatus::Closed);
    assert_eq!(descriptor.root_session_id.as_deref(), Some("root"));
    assert_eq!(descriptor.parent_session_id.as_deref(), Some("parent"));
    assert_eq!(descriptor.branch_id.as_deref(), Some("branch"));
}

#[test]
fn sqlite_close_rolls_back_descriptor_and_cancellations_on_write_failure() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("store.sqlite");
    let store = SqliteSessionStore::open(&path).unwrap();
    store.admit_task(request("first", "b", 0)).unwrap();
    let mut second = request("second", "c", 1);
    second.retry = None;
    store.admit_task(second).unwrap();
    let before = store.task_events(None, None).unwrap();
    assert_eq!(before.len(), 4);
    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM agent_tasks", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    conn.execute_batch("CREATE TRIGGER reject_second_cancel BEFORE UPDATE ON agent_tasks WHEN NEW.id = 'second' AND NEW.status = 'CANCELED' BEGIN SELECT RAISE(FAIL, 'fixture cancellation failure'); END;").unwrap();
    assert!(store
        .update_task_session(
            "session-one",
            TaskSessionUpdate {
                status: Some(SessionStatus::Closed),
                updated_at: "2026-10-12T00:00:00Z".into(),
                ..TaskSessionUpdate::default()
            }
        )
        .is_err());
    assert_eq!(
        store.get_task_session("session-one").unwrap().status,
        SessionStatus::Open
    );
    assert_eq!(store.get_task("first").unwrap().status, TaskStatus::Working);
    assert_eq!(
        store.get_task("second").unwrap().status,
        TaskStatus::Working
    );
    assert_eq!(store.task_events(None, None).unwrap(), before);
    assert_eq!(
        store.next_control_event_id().unwrap(),
        "event_0000000000000005"
    );
}

#[test]
fn memory_close_stages_fallible_cancellations_before_publication() {
    struct FailSecond {
        armed: AtomicBool,
        seen: AtomicUsize,
    }
    impl EventRedactor for FailSecond {
        fn redact_json_in_place(&self, value: &mut serde_json::Value) {
            if self.armed.load(Ordering::SeqCst)
                && value.as_str() == Some("fixture-failure")
                && self.seen.fetch_add(1, Ordering::SeqCst) == 1
            {
                *value = json!({"invalid":"failure shape"});
            }
        }
        fn redact_headers(&self, headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
            headers.clone()
        }
    }
    let redactor = Arc::new(FailSecond {
        armed: AtomicBool::new(false),
        seen: AtomicUsize::new(0),
    });
    let store = MemorySessionStore::with_hooks(StoreHooks {
        redaction: Some(redactor.clone()),
        ..StoreHooks::default()
    });
    for id in ["first", "second"] {
        let mut admission = request(id, "b", 0);
        admission.retry = None;
        admission.task.failure = Some(TaskFailure {
            code: "fixture".into(),
            message: "fixture-failure".into(),
        });
        store.admit_task(admission).unwrap();
    }
    let before = store.task_events(None, None).unwrap();
    assert_eq!(before.len(), 4);
    redactor.armed.store(true, Ordering::SeqCst);
    assert!(store
        .update_task_session(
            "session-one",
            TaskSessionUpdate {
                status: Some(SessionStatus::Closed),
                updated_at: "2026-10-12T00:00:00Z".into(),
                ..TaskSessionUpdate::default()
            }
        )
        .is_err());
    assert_eq!(redactor.seen.load(Ordering::SeqCst), 2);
    redactor.armed.store(false, Ordering::SeqCst);
    assert_eq!(
        store.get_task_session("session-one").unwrap().status,
        SessionStatus::Open
    );
    assert_eq!(store.get_task("first").unwrap().status, TaskStatus::Working);
    assert_eq!(
        store.get_task("second").unwrap().status,
        TaskStatus::Working
    );
    assert_eq!(store.task_events(None, None).unwrap(), before);
    assert_eq!(
        store.next_control_event_id().unwrap(),
        "event_0000000000000005"
    );
}
