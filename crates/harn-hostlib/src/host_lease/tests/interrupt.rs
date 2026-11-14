use std::cell::Cell;

use super::*;

#[test]
fn immediate_acquire_reports_registry_contention_without_enqueuing() {
    let temp = TempDir::new().unwrap();
    let (store, _clock) = paused_store(&temp);
    let mut writer = Connection::open(&store.db_path).unwrap();
    let transaction = writer
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let receipt = store
        .acquire_wait_interruptible(request("immediate"), Duration::ZERO, &|| false)
        .unwrap();
    assert_eq!(receipt.status, HostLeaseAcquireStatus::Deferred);
    assert_eq!(
        receipt.defer.unwrap().deferred_reason,
        HostLeaseDeferReason::RegistryBusy
    );
    assert!(receipt.queue.is_none());
    transaction.commit().unwrap();
    assert!(store.status("mac-local").unwrap().pending.is_empty());
}

#[test]
fn immediate_acquire_does_not_require_a_watchable_wake_file() {
    let temp = TempDir::new().unwrap();
    let (store, _clock) = paused_store(&temp);
    std::fs::remove_file(&store.wake_path).unwrap();
    let receipt = store
        .acquire_wait_interruptible(request("immediate"), Duration::ZERO, &|| false)
        .unwrap();
    assert_eq!(receipt.status, HostLeaseAcquireStatus::Acquired);
    assert_eq!(
        store.status("mac-local").unwrap().active.unwrap().owner,
        "immediate"
    );
}

#[test]
fn watcher_failure_retires_the_observed_waiter() {
    let temp = TempDir::new().unwrap();
    let (store, _clock) = paused_store(&temp);
    let holder = store
        .try_acquire(request("holder"))
        .unwrap()
        .handle
        .unwrap();
    let (events, notifications) = mpsc::channel();
    events
        .send(Err(notify::Error::generic("injected watcher failure")))
        .unwrap();
    let admitted = Cell::new(false);
    let mut progress = |_receipt: &HostLeaseAcquireReceipt| {
        assert_eq!(store.status("mac-local").unwrap().pending.len(), 1);
        admitted.set(true);
    };
    let result = store.acquire_wait_notified(
        request("failed-watch"),
        Duration::from_secs(5),
        waiter("failed-watch", store.now_ms().unwrap()),
        Some(&mut progress),
        None,
        &notifications,
    );
    assert!(
        admitted.get(),
        "the failure must happen after queue admission"
    );
    assert!(
        matches!(result, Err(HostLeaseError::Watch(message)) if message.contains("injected watcher failure"))
    );
    let state = store.status("mac-local").unwrap();
    assert!(state.pending.is_empty());
    assert_eq!(state.active.unwrap().lease_id, holder.lease_id);
}

#[test]
fn interrupted_wait_removes_its_observed_queue_entry() {
    let temp = TempDir::new().unwrap();
    let (store, _clock) = paused_store(&temp);
    let holder = store
        .try_acquire(request("holder"))
        .unwrap()
        .handle
        .unwrap();
    let checks = Cell::new(0);
    let interrupted = || {
        checks.set(checks.get() + 1);
        if checks.get() == 2 {
            let state = store.status("mac-local").unwrap();
            assert_eq!(state.pending.len(), 1, "the wait must reach admission");
            assert_eq!(state.active.unwrap().lease_id, holder.lease_id);
            true
        } else {
            false
        }
    };
    let result = store.acquire_wait_interruptible(
        request("cancelled"),
        Duration::from_secs(5),
        &interrupted,
    );
    assert!(matches!(result, Err(HostLeaseError::Interrupted)));
    let state = store.status("mac-local").unwrap();
    assert!(state.pending.is_empty());
    assert_eq!(state.active.unwrap().lease_id, holder.lease_id);
    store.release("mac-local", &holder.lease_id).unwrap();
    assert_eq!(
        store.try_acquire(request("successor")).unwrap().status,
        HostLeaseAcquireStatus::Acquired
    );
}

#[test]
fn interrupted_grant_is_released_before_ownership_is_returned() {
    for wait in [Duration::ZERO, Duration::from_secs(5)] {
        let temp = TempDir::new().unwrap();
        let (store, _clock) = paused_store(&temp);
        let checks = Cell::new(0);
        let interrupted = || {
            checks.set(checks.get() + 1);
            if checks.get() == 2 {
                assert_eq!(
                    store.status("mac-local").unwrap().active.unwrap().owner,
                    "cancelled"
                );
                true
            } else {
                false
            }
        };
        assert!(matches!(
            store.acquire_wait_interruptible(request("cancelled"), wait, &interrupted),
            Err(HostLeaseError::Interrupted)
        ));
        let state = store.status("mac-local").unwrap();
        assert!(state.pending.is_empty());
        assert!(state.active.is_none());
        assert_eq!(
            store.try_acquire(request("successor")).unwrap().status,
            HostLeaseAcquireStatus::Acquired
        );
    }
}
