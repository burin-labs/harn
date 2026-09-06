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
