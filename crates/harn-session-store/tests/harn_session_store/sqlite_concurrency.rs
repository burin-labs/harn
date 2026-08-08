//! Multi-process-style SQLite initialization and write coordination tests.

use std::sync::Arc;

use harn_session_store::{
    AppendEvent, CreateSession, ListFilter, SessionEventKind, SessionStore, SqliteSessionStore,
};
use serde_json::json;
use tempfile::TempDir;

#[test]
fn sqlite_bootstrap_and_append_serialize_independent_connections() {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("concurrent-bootstrap.sqlite");
    let worker_count = 8;
    let barrier = Arc::new(std::sync::Barrier::new(worker_count));
    let workers = (0..worker_count)
        .map(|index| {
            let barrier = barrier.clone();
            let path = path.clone();
            std::thread::spawn(move || {
                barrier.wait();
                let store = SqliteSessionStore::open(&path).expect("open independent sqlite");
                let session_id = format!("concurrent-session-{index}");
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime")
                    .block_on(async {
                        store
                            .create(CreateSession {
                                id: Some(session_id.clone()),
                                ..CreateSession::default()
                            })
                            .await
                            .expect("create independent session");
                        store
                            .append(
                                &session_id,
                                AppendEvent::new(
                                    SessionEventKind::Message,
                                    json!({"index": index}),
                                ),
                            )
                            .await
                            .expect("append independent session")
                    });
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().expect("worker must finish");
    }

    let store = SqliteSessionStore::open(path).expect("reopen sqlite");
    let sessions = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(store.list(ListFilter::default()))
        .expect("list sessions");
    assert_eq!(sessions.len(), worker_count);
    assert!(sessions.iter().all(|session| session.event_count == 1));
}
