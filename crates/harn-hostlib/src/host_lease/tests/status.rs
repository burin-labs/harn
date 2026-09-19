use super::*;

fn resource(domain: &str) -> HostLeaseResourceKey {
    HostLeaseResourceKey {
        machine: "mac-local".to_string(),
        resource_class: HostLeaseResourceClass::RustHeavy,
        domain: domain.to_string(),
    }
}

#[test]
fn overview_observes_known_idle_resources_instead_of_an_empty_census() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let overview = store.status_overview("mac-local", None).unwrap();
    assert_eq!(
        overview.resources.len(),
        HOST_LEASE_RESOURCE_DEFINITIONS.len()
    );
    for state in overview.resources {
        assert_eq!(state.observed_at_ms, overview.observed_at_ms);
        assert_eq!(state.domain, DEFAULT_HOST_LEASE_DOMAIN);
        assert!(state.active.is_none());
        assert!(state.pending.is_empty());
    }
}

#[test]
fn status_and_admission_share_priority_order_and_resource_scope() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let resource = resource("verification");
    let low = waiter("low", 1);
    let high = waiter("high", 2);
    store
        .enqueue_waiter(
            &resource,
            HostLeasePriorityClass::Deferrable,
            &low,
            i64::MAX,
            None,
        )
        .unwrap();
    store
        .enqueue_waiter(
            &resource,
            HostLeasePriorityClass::Interactive,
            &high,
            i64::MAX,
            None,
        )
        .unwrap();
    let mut other = resource.clone();
    other.domain = "other-domain".to_string();
    store
        .enqueue_waiter(
            &other,
            HostLeasePriorityClass::Measurement,
            &waiter("other", 3),
            i64::MAX,
            None,
        )
        .unwrap();
    other.machine = "other-host".to_string();
    store
        .enqueue_waiter(
            &other,
            HostLeasePriorityClass::Measurement,
            &waiter("other-host", 4),
            i64::MAX,
            None,
        )
        .unwrap();

    let scoped = store
        .status_for_domain(&resource.machine, resource.resource_class, &resource.domain)
        .unwrap();
    assert_eq!(
        scoped
            .pending
            .iter()
            .map(|entry| entry.waiter_id.as_str())
            .collect::<Vec<_>>(),
        ["high", "low"]
    );
    let overview = store.status_overview(&resource.machine, None).unwrap();
    assert_eq!(
        overview.resources.len(),
        HOST_LEASE_RESOURCE_DEFINITIONS.len() + 2
    );
    assert_eq!(
        overview
            .resources
            .iter()
            .map(|state| state.pending.len())
            .sum::<usize>(),
        3
    );
    let filtered = store
        .status_overview(&resource.machine, Some(&resource.domain))
        .unwrap();
    assert_eq!(
        filtered.resources.len(),
        HOST_LEASE_RESOURCE_DEFINITIONS.len()
    );
    assert!(filtered
        .resources
        .iter()
        .all(|state| state.domain == resource.domain));
    assert_eq!(
        filtered
            .resources
            .iter()
            .map(|state| state.pending.len())
            .sum::<usize>(),
        2
    );
    assert!(store.status(&resource.machine).unwrap().pending.is_empty());

    let mut request = request("high-owner");
    request.resource_class = resource.resource_class;
    request.domain = resource.domain.clone();
    request.priority_class = HostLeasePriorityClass::Interactive;
    let acquired = store
        .try_acquire_once(request, None, Some(i64::MAX), &high)
        .unwrap();
    assert_eq!(acquired.status, HostLeaseAcquireStatus::Acquired);
    assert_eq!(acquired.queue.unwrap().position, 1);
    let scoped = store
        .status_for_domain(&resource.machine, resource.resource_class, &resource.domain)
        .unwrap();
    assert_eq!(scoped.active.unwrap().owner, "high-owner");
    assert_eq!(scoped.pending.len(), 1);
    assert_eq!(scoped.pending[0].waiter_id, "low");
}

#[test]
fn status_preserves_unknown_waiters_and_removes_expired_or_dead_waiters() {
    let temp = TempDir::new().unwrap();
    let inspector = Arc::new(ScriptedProcessInspector::alive(42));
    let store = HostLeaseStore::for_root_with_inspector(
        temp.path(),
        Arc::clone(&inspector) as Arc<dyn ProcessInspector>,
    )
    .unwrap();
    let resource = resource(DEFAULT_HOST_LEASE_DOMAIN);
    store
        .enqueue_waiter(
            &resource,
            HostLeasePriorityClass::Measurement,
            &waiter("live", 1),
            i64::MAX,
            Some(123),
        )
        .unwrap();
    let read = || {
        store
            .status_for_resource(&resource.machine, resource.resource_class)
            .unwrap()
    };
    assert_eq!(read().pending.len(), 1);
    inspector.set(ProcessObservation::Unknown);
    assert_eq!(read().pending.len(), 1, "unobservable is not dead");
    inspector.set(ProcessObservation::Dead);
    assert!(read().pending.is_empty());

    inspector.set(ProcessObservation::Alive { identity: 42 });
    store
        .enqueue_waiter(
            &resource,
            HostLeasePriorityClass::Measurement,
            &waiter("reused", 2),
            i64::MAX,
            Some(123),
        )
        .unwrap();
    inspector.set(ProcessObservation::Alive { identity: 43 });
    assert!(
        read().pending.is_empty(),
        "PID reuse must not preserve a dead waiter"
    );
    store
        .enqueue_waiter(
            &resource,
            HostLeasePriorityClass::Measurement,
            &waiter("expires", 3),
            i64::MAX,
            None,
        )
        .unwrap();
    assert_eq!(read().pending.len(), 1);
    let expired = store
        .status_at(&resource.machine, resource.resource_class, i64::MAX)
        .unwrap();
    assert!(expired.pending.is_empty());
}

#[test]
fn pending_evidence_is_required_when_decoding_a_status_receipt() {
    let temp = TempDir::new().unwrap();
    let state = store(&temp).status("mac-local").unwrap();
    let mut encoded = serde_json::to_value(state).unwrap();
    encoded.as_object_mut().unwrap().remove("pending");
    assert!(serde_json::from_value::<HostLeaseState>(encoded).is_err());
}
