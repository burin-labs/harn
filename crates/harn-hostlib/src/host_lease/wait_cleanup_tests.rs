use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, OnceLock};

use super::*;

#[derive(Debug)]
enum CleanupEvent {
    Retried,
    Finished,
}

static BUSY_CALLS: AtomicUsize = AtomicUsize::new(0);
static EVENTS: OnceLock<mpsc::Sender<CleanupEvent>> = OnceLock::new();
static UNLOCKED: OnceLock<Arc<Barrier>> = OnceLock::new();

fn fail_first_busy_attempt(_prior_calls: i32) -> bool {
    if BUSY_CALLS.fetch_add(1, Ordering::SeqCst) == 0 {
        return false;
    }
    EVENTS.get().unwrap().send(CleanupEvent::Retried).unwrap();
    UNLOCKED.get().unwrap().wait();
    true
}

#[test]
fn a_raced_nonexpiring_grant_survives_a_busy_cleanup_attempt_until_release_is_confirmed() {
    let root = tempfile::tempdir().unwrap();
    let store = HostLeaseStore::for_root(root.path()).unwrap();
    let receipt = store
        .try_acquire(HostLeaseRequest {
            host: "cleanup-host".to_string(),
            resource_class: HostLeaseResourceClass::RustHeavy,
            domain: "cleanup-domain".to_string(),
            execution_context: None,
            owner: "cancelled-live-owner".to_string(),
            priority_class: HostLeasePriorityClass::Measurement,
            ttl_ms: None,
            owner_pid: Some(std::process::id()),
            reason: None,
            metadata: BTreeMap::new(),
        })
        .unwrap();
    assert_eq!(receipt.status, HostLeaseAcquireStatus::Acquired);
    assert!(receipt.handle.as_ref().unwrap().expires_at_ms.is_none());
    let mut writer = rusqlite::Connection::open(&store.db_path).unwrap();
    let transaction = writer
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let (events, observed) = mpsc::channel();
    EVENTS.set(events.clone()).unwrap();
    let unlocked = Arc::new(Barrier::new(2));
    UNLOCKED.set(Arc::clone(&unlocked)).unwrap();
    let cleanup_store = store.clone().with_busy_handler(fail_first_busy_attempt);
    let worker = harn_parser::runtime_stack::spawn(move || {
        let mut scope = AcquireScope::new(&cleanup_store, "raced-grant");
        scope.observe(&receipt);
        drop(scope);
        events.send(CleanupEvent::Finished).unwrap();
    });
    let first = harn_clock::test_support::recv_within("cleanup retry or completion", &observed);
    transaction.commit().unwrap();
    if matches!(first, CleanupEvent::Retried) {
        unlocked.wait();
        assert!(matches!(
            harn_clock::test_support::recv_within("confirmed grant retirement", &observed),
            CleanupEvent::Finished
        ));
    }
    worker.join().unwrap();
    assert!(
        matches!(first, CleanupEvent::Retried),
        "cleanup abandoned the nonexpiring grant after its first busy failure"
    );
    assert!(
        store
            .status_for_domain(
                "cleanup-host",
                HostLeaseResourceClass::RustHeavy,
                "cleanup-domain",
            )
            .unwrap()
            .active
            .is_none(),
        "the live owner must not retain the cancelled grant"
    );
}
