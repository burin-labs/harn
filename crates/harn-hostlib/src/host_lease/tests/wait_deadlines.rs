use super::*;

#[test]
fn supervised_wait_defers_once_the_injected_clock_passes_its_deadline() {
    let temp = TempDir::new().unwrap();
    let (store, clock) = paused_store(&temp);
    let holder = store
        .try_acquire(request("holder"))
        .unwrap()
        .handle
        .unwrap();
    let run = store
        .begin_run(
            "expiring-waiter",
            HostLeasePriorityClass::Measurement,
            HostLeaseResourceKey {
                machine: holder.host.clone(),
                resource_class: holder.resource_class,
                domain: holder.domain.clone(),
            },
            HostLeaseExecutionContext::cargo(Path::new("/workspace"), Path::new("/target"), None),
            1_000,
        )
        .unwrap();
    let connection = store.connection(SQLITE_MUTATION_BUSY_TIMEOUT).unwrap();
    let waiter_count = || {
        connection
            .query_row(
                "SELECT COUNT(*) FROM host_lease_waiters WHERE waiter_id = ?1",
                params![run.run_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
    };
    let mut started_at_ms = None;
    let mut report = |progress: &HostLeaseAcquireReceipt| {
        assert_eq!(progress.status, HostLeaseAcquireStatus::Deferred);
        if started_at_ms.is_none() {
            assert_eq!(waiter_count(), 1, "the waiter must have entered the queue");
            started_at_ms = Some(progress.observed_at_ms);
            // Expire between the admission snapshot and the wait decision.
            clock.advance(Duration::from_secs(2));
        }
    };
    let receipt = store
        .acquire_wait_for_run_with_progress(&run.run_id, std::process::id(), &mut report)
        .unwrap();
    let started_at_ms = started_at_ms.expect("the waiter must report its first deferral");
    assert_eq!(receipt.status, HostLeaseAcquireStatus::Deferred);
    assert_eq!(receipt.observed_at_ms, started_at_ms + 2_000);
    assert_eq!(receipt.waited_ms, 2_000);
    assert_eq!(waiter_count(), 0, "an expired waiter must leave the queue");
    let state = store.status(&holder.host).unwrap();
    assert_eq!(state.active.as_ref().unwrap().lease_id, holder.lease_id);
}
