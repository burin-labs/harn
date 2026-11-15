use std::cell::Cell;

use super::*;

fn hold_exclusive_writer(
    store: &HostLeaseStore,
) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let path = store.db_path.clone();
    let (locked, observed) = mpsc::channel();
    let (unlock, unlocked) = mpsc::channel();
    let writer = harn_parser::runtime_stack::spawn(move || {
        let mut connection = Connection::open(path).unwrap();
        connection
            .pragma_update(None, "locking_mode", "EXCLUSIVE")
            .unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Exclusive)
            .unwrap();
        locked.send(()).unwrap();
        harn_clock::test_support::recv_within("writer release", &unlocked);
        transaction.commit().unwrap();
    });
    harn_clock::test_support::recv_within("exclusive writer reached", &observed);
    (unlock, writer)
}

#[test]
fn immediate_acquire_defers_when_opening_the_registry_is_busy() {
    let temp = TempDir::new().unwrap();
    let (store, _clock) = paused_store(&temp);
    let mut writer = Connection::open(&store.db_path).unwrap();
    writer
        .pragma_update(None, "locking_mode", "EXCLUSIVE")
        .unwrap();
    let transaction = writer
        .transaction_with_behavior(TransactionBehavior::Exclusive)
        .unwrap();
    let blocked = store.connection(Duration::ZERO).unwrap_err();
    assert!(
        matches!(blocked, HostLeaseError::Database(error) if sqlite_is_busy(&error)),
        "the writer must reach connection-level contention"
    );
    let receipt = store
        .acquire_wait_interruptible(request("opening-busy"), Duration::ZERO, &|| false)
        .expect("connection contention must defer rather than become a backend error");
    assert_eq!(receipt.status, HostLeaseAcquireStatus::Deferred);
    assert_eq!(
        receipt.defer.unwrap().deferred_reason,
        HostLeaseDeferReason::RegistryBusy
    );
    assert!(receipt.queue.is_none());
    transaction.commit().unwrap();
    drop(writer);
    assert!(store.status("mac-local").unwrap().pending.is_empty());
}

#[test]
fn immediate_registry_busy_does_not_remove_an_unobserved_waiter() {
    let temp = TempDir::new().unwrap();
    let (store, _clock) = paused_store(&temp);
    let store = store.with_busy_handler(|_| false);
    let mut writer = Connection::open(&store.db_path).unwrap();
    let transaction = writer
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let receipt = store
        .try_acquire(request("immediate-busy"))
        .expect("an unqueued busy receipt has no waiter to delete");
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
fn connection_contention_preserves_wait_retry_cancellation_and_deadline() {
    #[derive(Clone, Copy)]
    enum Outcome {
        Acquire,
        Cancel,
        Deadline,
    }
    for outcome in [Outcome::Acquire, Outcome::Cancel, Outcome::Deadline] {
        let temp = TempDir::new().unwrap();
        let (store, clock) = paused_store(&temp);
        let (unlock, writer) = hold_exclusive_writer(&store);
        let mut unlock = Some(unlock);
        let mut writer = Some(writer);
        let (events, notifications) = mpsc::channel();
        let reached = Cell::new(false);
        let cancelled = Cell::new(false);
        let mut progress = |receipt: &HostLeaseAcquireReceipt| {
            reached.set(true);
            assert_eq!(
                receipt.defer.as_ref().unwrap().deferred_reason,
                HostLeaseDeferReason::RegistryBusy
            );
            assert!(receipt.queue.is_none());
            match outcome {
                Outcome::Acquire => {
                    unlock.take().unwrap().send(()).unwrap();
                    writer.take().unwrap().join().unwrap();
                }
                Outcome::Cancel => cancelled.set(true),
                Outcome::Deadline => clock.advance(Duration::from_secs(5)),
            }
            events
                .send(Ok(notify::Event::new(notify::EventKind::Any)))
                .unwrap();
        };
        let result = store.acquire_wait_notified(
            request("connection-wait"),
            Duration::from_secs(5),
            waiter("connection-wait", store.now_ms().unwrap()),
            Some(&mut progress),
            Some(&|| cancelled.get()),
            &notifications,
        );
        assert!(reached.get(), "the wait must observe the real held writer");
        match outcome {
            Outcome::Acquire => {
                let receipt = result.unwrap();
                assert_eq!(receipt.status, HostLeaseAcquireStatus::Acquired);
                store
                    .release_owned(receipt.handle.as_ref().unwrap())
                    .unwrap();
            }
            Outcome::Cancel => assert!(matches!(result, Err(HostLeaseError::Interrupted))),
            Outcome::Deadline => {
                let receipt = result.unwrap();
                assert_eq!(receipt.status, HostLeaseAcquireStatus::Deferred);
                assert_eq!(receipt.waited_ms, 5_000);
                assert_eq!(
                    receipt.defer.unwrap().deferred_reason,
                    HostLeaseDeferReason::RegistryBusy
                );
            }
        }
        if let Some(unlock) = unlock {
            unlock.send(()).unwrap();
        }
        if let Some(writer) = writer {
            writer.join().unwrap();
        }
        let state = store.status("mac-local").unwrap();
        assert!(state.active.is_none());
        assert!(state.pending.is_empty());
        assert_eq!(
            store.try_acquire(request("successor")).unwrap().status,
            HostLeaseAcquireStatus::Acquired
        );
    }
}

#[test]
fn non_contention_connection_errors_remain_fatal() {
    let temp = TempDir::new().unwrap();
    let (store, _clock) = paused_store(&temp);
    std::fs::write(&store.db_path, b"not a SQLite registry").unwrap();
    let result =
        store.acquire_wait_interruptible(request("broken-registry"), Duration::ZERO, &|| false);
    assert!(matches!(result, Err(HostLeaseError::Database(error)) if !sqlite_is_busy(&error)));
}

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
