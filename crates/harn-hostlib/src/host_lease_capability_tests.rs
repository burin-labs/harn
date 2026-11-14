use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier, OnceLock};

use rusqlite::{Connection, TransactionBehavior};

use super::*;

enum ReleaseEvent {
    Retried,
    Finished(Result<VmValue, harn_vm::VmError>),
}

static BUSY_CALLS: AtomicUsize = AtomicUsize::new(0);
static RELEASE_EVENTS: OnceLock<mpsc::Sender<ReleaseEvent>> = OnceLock::new();
static UNLOCKED: OnceLock<Arc<Barrier>> = OnceLock::new();

fn fail_first_release_attempt(_prior_calls: i32) -> bool {
    if BUSY_CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
        return false;
    }
    RELEASE_EVENTS
        .get()
        .unwrap()
        .send(ReleaseEvent::Retried)
        .unwrap();
    UNLOCKED.get().unwrap().wait();
    true
}

#[test]
fn delivered_nonexpiring_guard_retains_cleanup_through_a_busy_release() {
    let root = tempfile::tempdir().unwrap();
    let store = HostLeaseStore::for_root(root.path()).unwrap();
    let receipt = store
        .try_acquire(HostLeaseRequest {
            host: "guard-host".into(),
            resource_class: HostLeaseResourceClass::RustHeavy,
            domain: "guard-domain".into(),
            execution_context: None,
            owner: "delivered-live-owner".into(),
            priority_class: HostLeasePriorityClass::Measurement,
            ttl_ms: None,
            owner_pid: Some(std::process::id()),
            reason: None,
            metadata: BTreeMap::new(),
        })
        .unwrap();
    assert!(receipt.handle.as_ref().unwrap().expires_at_ms.is_none());
    let delivered = acquire_to_value(
        store.clone().with_busy_handler(fail_first_release_attempt),
        receipt,
    )
    .unwrap();
    let Some(VmValue::ResourceGuard(guard)) = delivered.as_dict().unwrap().get("guard") else {
        panic!("acquisition must deliver the actual native resource guard");
    };
    let guard = Arc::clone(guard);
    assert!(!guard.is_released());
    drop(delivered);
    let mut writer = Connection::open(root.path().join("host-leases.sqlite")).unwrap();
    let transaction = writer
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let (events, observed) = mpsc::channel();
    RELEASE_EVENTS
        .set(events.clone())
        .unwrap_or_else(|_| panic!("one release fixture"));
    let unlocked = Arc::new(Barrier::new(2));
    UNLOCKED.set(Arc::clone(&unlocked)).unwrap();
    let worker = harn_parser::runtime_stack::spawn(move || {
        events
            .send(ReleaseEvent::Finished(guard.release()))
            .unwrap();
    });
    let first =
        harn_clock::test_support::recv_within("delivered guard retry or completion", &observed);
    transaction.commit().unwrap();
    let result = match first {
        ReleaseEvent::Retried => {
            unlocked.wait();
            match harn_clock::test_support::recv_within(
                "confirmed delivered guard retirement",
                &observed,
            ) {
                ReleaseEvent::Finished(result) => result,
                ReleaseEvent::Retried => panic!("unexpected additional retry"),
            }
        }
        ReleaseEvent::Finished(result) => result,
    };
    worker.join().unwrap();
    let active = store
        .status_for_domain(
            "guard-host",
            HostLeaseResourceClass::RustHeavy,
            "guard-domain",
        )
        .unwrap()
        .active;
    assert!(
        active.is_none(),
        "the delivered guard stranded a live nonexpiring lease after release: {result:?}"
    );
    assert!(
        result.is_ok(),
        "release must return its confirmed receipt: {result:?}"
    );
}

mod abandonment {
    use notify::Watcher;

    use super::*;

    enum Event {
        Returned,
        Retried,
    }
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    static EVENTS: OnceLock<mpsc::Sender<Event>> = OnceLock::new();
    static UNLOCKED: OnceLock<Arc<Barrier>> = OnceLock::new();

    fn fail_first_cleanup_attempt(_prior_calls: i32) -> bool {
        if CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
            return false;
        }
        EVENTS.get().unwrap().send(Event::Retried).unwrap();
        UNLOCKED.get().unwrap().wait();
        true
    }

    #[test]
    fn abandoned_guard_returns_before_the_writer_releases_and_confirms_retirement() {
        let root = tempfile::tempdir().unwrap();
        let store = HostLeaseStore::for_root(root.path()).unwrap();
        let receipt = store
            .try_acquire(HostLeaseRequest {
                host: "abandon-host".into(),
                resource_class: HostLeaseResourceClass::RustHeavy,
                domain: "abandon-domain".into(),
                execution_context: None,
                owner: "abandoned-live-owner".into(),
                priority_class: HostLeasePriorityClass::Measurement,
                ttl_ms: None,
                owner_pid: Some(std::process::id()),
                reason: None,
                metadata: BTreeMap::new(),
            })
            .unwrap();
        assert!(receipt.handle.as_ref().unwrap().expires_at_ms.is_none());
        let delivered = acquire_to_value(
            store.clone().with_busy_handler(fail_first_cleanup_attempt),
            receipt,
        )
        .unwrap();
        let mut writer = Connection::open(root.path().join("host-leases.sqlite")).unwrap();
        let transaction = writer
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let (retired, retirement) = mpsc::channel();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if event.is_ok_and(|event| event.kind.is_modify()) {
                    let _ = retired.send(());
                }
            })
            .unwrap();
        watcher
            .watch(
                &root.path().join("host-leases.wake"),
                notify::RecursiveMode::NonRecursive,
            )
            .unwrap();
        let (events, observed) = mpsc::channel();
        EVENTS
            .set(events.clone())
            .unwrap_or_else(|_| panic!("one abandonment fixture"));
        let unlocked = Arc::new(Barrier::new(2));
        UNLOCKED.set(Arc::clone(&unlocked)).unwrap();
        let worker = harn_parser::runtime_stack::spawn(move || {
            drop(delivered);
            events.send(Event::Returned).unwrap();
        });
        let mut returned = false;
        let mut retried = false;
        for _ in 0..2 {
            match harn_clock::test_support::recv_within(
                "abandonment return and retained cleanup",
                &observed,
            ) {
                Event::Returned => returned = true,
                Event::Retried => retried = true,
            }
        }
        assert!(
            returned && retried,
            "teardown must return while the held writer still blocks cleanup"
        );
        transaction.commit().unwrap();
        unlocked.wait();
        worker.join().unwrap();
        harn_clock::test_support::recv_within(
            "physical lease retirement notification",
            &retirement,
        );
        assert!(store
            .status_for_domain(
                "abandon-host",
                HostLeaseResourceClass::RustHeavy,
                "abandon-domain"
            )
            .unwrap()
            .active
            .is_none());
    }
}
