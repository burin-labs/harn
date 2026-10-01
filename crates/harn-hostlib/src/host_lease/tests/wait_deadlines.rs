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
    let mut started_at_ms = None;
    let mut report = |progress: &HostLeaseAcquireReceipt| {
        assert_eq!(progress.status, HostLeaseAcquireStatus::Deferred);
        if started_at_ms.is_none() {
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
    let state = store.status(&holder.host).unwrap();
    assert_eq!(state.active.as_ref().unwrap().lease_id, holder.lease_id);
    let encoded = serde_json::to_value(&state).unwrap();
    assert!(
        encoded["pending"]
            .as_array()
            .is_none_or(|pending| pending.is_empty()),
        "an expired waiter must leave the queue: {encoded}"
    );
}
