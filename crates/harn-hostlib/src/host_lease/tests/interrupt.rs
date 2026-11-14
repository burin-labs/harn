use std::cell::Cell;

use super::*;

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
