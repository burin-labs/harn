use std::sync::Arc;

use super::*;

struct NoopSessionObserver;

impl harn_session_store::SessionChangeObserver for NoopSessionObserver {
    fn session_updated(&self, _meta: &harn_session_store::SessionMeta) {}
}

#[tokio::test]
async fn canonical_maintenance_open_preserves_hooks_and_watch_ownership() {
    let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
    let root = tempfile::tempdir().expect("store root");
    let _subscription = crate::subscribe_session_changes(Arc::new(NoopSessionObserver));
    let store = open_canonical_store_for_maintenance(root.path())
        .expect("open canonical maintenance store");

    assert!(store.hooks().redaction.is_some());
    assert!(store.hooks().change_observer.is_some());
    assert_eq!(
        crate::stdlib::session_wal_watch::watcher_count_for(store.path()),
        1,
        "canonical maintenance handle owns one live WAL watcher"
    );
    let database = store.path().to_path_buf();
    drop(store);
    assert_eq!(
        crate::stdlib::session_wal_watch::watcher_count_for(&database),
        0,
        "dropping maintenance handle releases its watcher"
    );
}

#[tokio::test]
async fn closed_checkpoint_inspection_does_not_start_a_watcher() {
    use harn_session_store::{CreateSession, SessionStore};

    let _bus = crate::stdlib::session_change::test_support::exclusive_bus().await;
    let root = tempfile::tempdir().expect("store root");
    let _subscription = crate::subscribe_session_changes(Arc::new(NoopSessionObserver));
    let database = store_path(&SessionStoreDir::under_root(root.path()));
    let before_control = crate::stdlib::session_wal_watch::watcher_starts_for(&database);
    let store = open_canonical_store(root.path()).expect("open canonical store");
    store
        .create(CreateSession {
            id: Some("stop-checkpoint".into()),
            ..Default::default()
        })
        .await
        .expect("create session");
    let after_control = crate::stdlib::session_wal_watch::watcher_starts_for(&database);
    assert!(
        after_control > before_control,
        "the same subscription must start a normal store watcher"
    );
    drop(store);

    let checkpoint = crate::agent_session_journal::inspect_closed_session_checkpoint(
        "stop-checkpoint",
        Some(SessionStoreDir::under_root(root.path())),
        |_| true,
    )
    .await
    .expect("inspect checkpoint")
    .expect("session exists");
    assert_eq!(checkpoint.meta.id, "stop-checkpoint");
    assert_eq!(
        crate::stdlib::session_wal_watch::watcher_starts_for(&database),
        after_control,
        "the transient checkpoint read must not start and synchronously join a watcher"
    );
}
