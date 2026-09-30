use super::*;

#[test]
fn supervised_wait_defers_once_the_injected_clock_passes_its_deadline() {
    let temp = TempDir::new().unwrap();
    let (store, clock) = paused_store(&temp);
    let store = Arc::new(store);
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
    let (progress_tx, progress_rx) = mpsc::channel();
    let waiter = {
        let store = Arc::clone(&store);
        let run_id = run.run_id;
        thread::spawn(move || {
            let mut report = |receipt: &HostLeaseAcquireReceipt| {
                let _ = progress_tx.send(receipt.clone());
            };
            store
                .acquire_wait_for_run_with_progress(&run_id, std::process::id(), &mut report)
                .unwrap()
        })
    };
    // The first deferral proves the waiter entered its wait loop.
    let progress = progress_rx.recv().unwrap();
    assert_eq!(progress.status, HostLeaseAcquireStatus::Deferred);
    let started_at_ms = progress.observed_at_ms;

    clock.advance(Duration::from_secs(2));
    store.signal_waiters();

    let receipt = waiter.join().unwrap();
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
