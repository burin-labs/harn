use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, OnceLock};
use std::thread;

use tempfile::TempDir;

use super::*;

fn store(temp: &TempDir) -> HostLeaseStore {
    HostLeaseStore::for_root(temp.path()).unwrap()
}

#[derive(Debug)]
struct ScriptedProcessInspector {
    observation: AtomicU64,
}

impl ScriptedProcessInspector {
    fn alive(identity: u64) -> Self {
        Self {
            observation: AtomicU64::new(identity.saturating_add(2)),
        }
    }

    fn set(&self, observation: ProcessObservation) {
        let value = match observation {
            ProcessObservation::Unknown => 0,
            ProcessObservation::Dead => 1,
            ProcessObservation::Alive { identity } => identity.saturating_add(2),
        };
        self.observation.store(value, Ordering::SeqCst);
    }
}

impl ProcessInspector for ScriptedProcessInspector {
    fn observe(&self, _pid: u32) -> ProcessObservation {
        match self.observation.load(Ordering::SeqCst) {
            0 => ProcessObservation::Unknown,
            1 => ProcessObservation::Dead,
            value => ProcessObservation::Alive {
                identity: value - 2,
            },
        }
    }
}

fn request(owner: &str) -> HostLeaseRequest {
    HostLeaseRequest {
        host: "mac-local".to_string(),
        resource_class: HostLeaseResourceClass::WholeMachine,
        domain: DEFAULT_HOST_LEASE_DOMAIN.to_string(),
        execution_context: None,
        owner: owner.to_string(),
        priority_class: HostLeasePriorityClass::Measurement,
        ttl_ms: Some(60_000),
        owner_pid: None,
        reason: Some("meter run".to_string()),
        metadata: BTreeMap::new(),
    }
}

#[test]
fn acquire_blocks_second_owner_until_release() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let first = store
        .try_acquire_at(request("codex-0"), 1_000, None, 0)
        .unwrap();
    assert_eq!(first.status, HostLeaseAcquireStatus::Acquired);
    let second = store
        .try_acquire_at(request("codex-1"), 1_001, None, 0)
        .unwrap();
    assert_eq!(second.status, HostLeaseAcquireStatus::Deferred);
    assert_eq!(
        second.defer.as_ref().unwrap().deferred_reason,
        HostLeaseDeferReason::Contended
    );

    let handle = first.handle.unwrap();
    assert!(
        store
            .release(&handle.host, &handle.lease_id)
            .unwrap()
            .released
    );
    let third = store
        .try_acquire_at(request("codex-1"), 1_002, None, 0)
        .unwrap();
    assert_eq!(third.status, HostLeaseAcquireStatus::Acquired);
}

#[test]
fn immediate_transaction_allows_at_most_one_race_winner() {
    let temp = TempDir::new().unwrap();
    let store = Arc::new(store(&temp));
    let worker_count = 12;
    let barrier = Arc::new(Barrier::new(worker_count));
    let workers = (0..worker_count)
        .map(|index| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                store
                    .try_acquire(request(&format!("worker-{index}")))
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let receipts = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    let acquired = receipts
        .iter()
        .filter(|receipt| receipt.status == HostLeaseAcquireStatus::Acquired)
        .count();
    assert!(acquired <= 1);
    assert!(receipts.iter().all(|receipt| {
        receipt.status == HostLeaseAcquireStatus::Acquired
            || matches!(
                receipt.defer.as_ref().map(|defer| defer.deferred_reason),
                Some(HostLeaseDeferReason::Contended) | Some(HostLeaseDeferReason::RegistryBusy)
            )
    }));

    let recovery = store.try_acquire(request("recovery")).unwrap();
    assert_eq!(
        acquired + usize::from(recovery.status == HostLeaseAcquireStatus::Acquired),
        1
    );
    if acquired == 0 {
        assert_eq!(recovery.status, HostLeaseAcquireStatus::Acquired);
    } else {
        assert_eq!(recovery.status, HostLeaseAcquireStatus::Deferred);
    }
    if recovery.status == HostLeaseAcquireStatus::Deferred {
        assert!(matches!(
            recovery.defer.as_ref().map(|defer| defer.deferred_reason),
            Some(HostLeaseDeferReason::Contended)
        ));
    }
}

#[test]
fn resource_classes_are_independently_exclusive() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);

    let whole_machine = store
        .try_acquire_at(request("whole-machine"), 1_000, None, 0)
        .unwrap();
    assert_eq!(whole_machine.status, HostLeaseAcquireStatus::Acquired);

    let mut first_rust = request("rust-first");
    first_rust.resource_class = HostLeaseResourceClass::RustHeavy;
    let first_rust = store.try_acquire_at(first_rust, 1_001, None, 0).unwrap();
    assert_eq!(first_rust.status, HostLeaseAcquireStatus::Acquired);

    let mut second_rust = request("rust-second");
    second_rust.resource_class = HostLeaseResourceClass::RustHeavy;
    let second_rust = store.try_acquire_at(second_rust, 1_002, None, 0).unwrap();
    assert_eq!(second_rust.status, HostLeaseAcquireStatus::Deferred);
    assert_eq!(
        second_rust.defer.unwrap().resource_class,
        HostLeaseResourceClass::RustHeavy
    );
    assert_eq!(HostLeaseResourceClass::RustHeavy.capacity(), 1);
    assert_eq!(
        HostLeaseResourceClass::RustHeavy.definition().name,
        "rust-heavy"
    );
}

#[test]
fn named_domains_are_independently_exclusive() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut first = request("release-owner");
    first.domain = "release".to_string();
    let first = store.try_acquire_at(first, 1_000, None, 0).unwrap();
    assert_eq!(first.status, HostLeaseAcquireStatus::Acquired);

    let mut contended = request("release-contender");
    contended.domain = "release".to_string();
    let contended = store.try_acquire_at(contended, 1_001, None, 0).unwrap();
    assert_eq!(contended.status, HostLeaseAcquireStatus::Deferred);
    assert_eq!(contended.defer.unwrap().domain, "release");

    let mut build = request("build-owner");
    build.domain = "build".to_string();
    let build = store.try_acquire_at(build, 1_002, None, 0).unwrap();
    assert_eq!(build.status, HostLeaseAcquireStatus::Acquired);
    assert_eq!(build.handle.unwrap().domain, "build");
}

#[test]
fn domains_are_validated_before_registry_access() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    for domain in ["", ".", "..", "release/owner", "release owner"] {
        let mut invalid = request("invalid-domain");
        invalid.domain = domain.to_string();
        assert!(matches!(
            store.try_acquire_at(invalid, 1_000, None, 0),
            Err(HostLeaseError::InvalidRequest(_))
        ));
    }
    let mut oversized = request("invalid-domain");
    oversized.domain = "x".repeat(129);
    assert!(matches!(
        store.try_acquire_at(oversized, 1_000, None, 0),
        Err(HostLeaseError::InvalidRequest(_))
    ));
}

#[test]
fn typed_execution_context_round_trips_without_open_metadata() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let context = HostLeaseExecutionContext::cargo(
        Path::new("/workspace/project"),
        Path::new("/tmp/target"),
        Some(Path::new("/tmp/build")),
    );
    let mut request = request("cargo-runner");
    request.resource_class = HostLeaseResourceClass::RustHeavy;
    request.execution_context = Some(context.clone());

    let acquired = store.try_acquire_at(request, 1_000, None, 0).unwrap();
    assert_eq!(
        acquired
            .handle
            .as_ref()
            .and_then(|handle| handle.execution_context.as_ref()),
        Some(&context)
    );
    let state = store
        .status_at("mac-local", HostLeaseResourceClass::RustHeavy, 1_001)
        .unwrap();
    assert_eq!(
        state
            .active
            .as_ref()
            .and_then(|handle| handle.execution_context.as_ref()),
        Some(&context)
    );
}

#[test]
fn run_receipts_persist_redacted_context_and_validate_state_edges() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let context = HostLeaseExecutionContext::cargo(
        Path::new("/private/workspace/secret-project"),
        Path::new("/private/build/target"),
        None,
    );
    let receipt = store
        .begin_run(
            "cargo-runner",
            HostLeasePriorityClass::Interactive,
            HostLeaseResourceKey {
                machine: "mac-local".to_string(),
                resource_class: HostLeaseResourceClass::RustHeavy,
                domain: DEFAULT_HOST_LEASE_DOMAIN.to_string(),
            },
            context,
            30_000,
        )
        .unwrap();

    assert_eq!(receipt.schema_version, RUN_RECEIPT_SCHEMA_VERSION);
    assert_eq!(receipt.owner, "cargo-runner");
    assert_eq!(receipt.priority_class, HostLeasePriorityClass::Interactive);
    assert_eq!(receipt.wait_limit_ms, 30_000);
    assert_eq!(store.load_run(&receipt.run_id).unwrap(), receipt);
    let persisted =
        std::fs::read_to_string(store.run_receipt_path(&receipt.run_id).unwrap()).unwrap();
    assert!(!persisted.contains("secret-project"));
    assert!(!persisted.contains("/private/build"));

    store
        .transition_run(
            &receipt.run_id,
            HostLeaseRunState::Running {
                lease_id: "lease-1".to_string(),
                acquired_at_ms: 1_000,
                acquire_wait_ms: 3,
                worker_pid: 42,
            },
        )
        .unwrap();
    let mismatched = store
        .transition_run(
            &receipt.run_id,
            HostLeaseRunState::Completed {
                lease_id: "different-lease".to_string(),
                acquire_wait_ms: 3,
                hold_ms: 10,
                worker_pid: 42,
                exit: HostLeaseProcessExit {
                    code: Some(0),
                    signal: None,
                },
                release: HostLeaseRunReleaseOutcome::Released,
                finished_at_ms: 1_010,
            },
        )
        .unwrap_err();
    assert!(matches!(mismatched, HostLeaseError::InvalidRequest(_)));
    store
        .transition_run(
            &receipt.run_id,
            HostLeaseRunState::Completed {
                lease_id: "lease-1".to_string(),
                acquire_wait_ms: 3,
                hold_ms: 10,
                worker_pid: 42,
                exit: HostLeaseProcessExit {
                    code: Some(0),
                    signal: None,
                },
                release: HostLeaseRunReleaseOutcome::Released,
                finished_at_ms: 1_010,
            },
        )
        .unwrap();
    let error = store
        .transition_run(
            &receipt.run_id,
            HostLeaseRunState::Deferred {
                observed_at_ms: 1_011,
                waited_ms: 4,
            },
        )
        .unwrap_err();
    assert!(matches!(error, HostLeaseError::InvalidRequest(_)));
}

#[test]
fn legacy_table_migrates_to_the_whole_machine_resource() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join(LEASE_DB_FILE);
    let connection = Connection::open(database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE host_leases (
                    host TEXT PRIMARY KEY NOT NULL,
                    lease_id TEXT NOT NULL,
                    owner TEXT NOT NULL,
                    priority_class TEXT NOT NULL,
                    acquired_at_ms INTEGER NOT NULL,
                    updated_at_ms INTEGER NOT NULL,
                    expires_at_ms INTEGER,
                    owner_pid INTEGER,
                    owner_process_identity INTEGER,
                    reason TEXT,
                    metadata_json TEXT NOT NULL
                );
                INSERT INTO host_leases VALUES (
                    'mac-local', 'legacy-token', 'legacy-owner', 'measurement',
                    1000, 1000, 61000, NULL, NULL, 'legacy migration', '{}'
                );",
        )
        .unwrap();
    drop(connection);

    let store = store(&temp);
    let migrated = store
        .status_at("mac-local", HostLeaseResourceClass::WholeMachine, 1_001)
        .unwrap();
    let handle = migrated.active.expect("legacy row remains active");
    assert_eq!(handle.lease_id, "legacy-token");
    assert_eq!(handle.resource_class, HostLeaseResourceClass::WholeMachine);

    let mut rust = request("rust-owner");
    rust.resource_class = HostLeaseResourceClass::RustHeavy;
    assert_eq!(
        store.try_acquire_at(rust, 1_002, None, 0).unwrap().status,
        HostLeaseAcquireStatus::Acquired
    );
}

#[test]
fn prior_resource_table_migrates_into_the_default_domain() {
    let temp = TempDir::new().unwrap();
    let database = temp.path().join(LEASE_DB_FILE);
    let connection = Connection::open(database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE host_leases (
                host TEXT NOT NULL,
                resource_class TEXT NOT NULL,
                lease_id TEXT NOT NULL,
                owner TEXT NOT NULL,
                priority_class TEXT NOT NULL,
                acquired_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                expires_at_ms INTEGER,
                owner_pid INTEGER,
                owner_process_identity INTEGER,
                reason TEXT,
                metadata_json TEXT NOT NULL,
                execution_context_json TEXT,
                PRIMARY KEY (host, resource_class)
            );
            INSERT INTO host_leases VALUES (
                'mac-local', 'rust-heavy', 'v2-token', 'v2-owner', 'measurement',
                1000, 1000, 61000, NULL, NULL, 'v2 migration', '{\"lane\":\"p7\"}', NULL
            );",
        )
        .unwrap();
    drop(connection);

    let store = store(&temp);
    let migrated = store
        .status_at_domain(
            "mac-local",
            HostLeaseResourceClass::RustHeavy,
            DEFAULT_HOST_LEASE_DOMAIN,
            1_001,
        )
        .unwrap()
        .active
        .expect("v2 row remains active");
    assert_eq!(migrated.domain, DEFAULT_HOST_LEASE_DOMAIN);
    assert_eq!(migrated.lease_id, "v2-token");
    assert_eq!(
        migrated.metadata.get("lane").map(String::as_str),
        Some("p7")
    );
}

#[test]
fn expiry_is_recovered_inside_the_acquire_transaction() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut short = request("codex-0");
    short.ttl_ms = Some(10);
    short
        .metadata
        .insert("version".to_string(), "1.2.3".to_string());
    let prior = store
        .try_acquire_at(short, 1_000, None, 0)
        .unwrap()
        .handle
        .unwrap();
    let next = store
        .try_acquire_at(request("codex-1"), 1_011, None, 0)
        .unwrap();
    assert_eq!(next.status, HostLeaseAcquireStatus::Acquired);
    assert!(next.recovered_stale_lease);
    assert_eq!(next.recovered, Some(prior));
}

#[test]
fn status_returns_the_exact_recovered_handle() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut short = request("release-owner");
    short.domain = "release".to_string();
    short.ttl_ms = Some(10);
    short
        .metadata
        .insert("version".to_string(), "1.2.3".to_string());
    let prior = store
        .try_acquire_at(short, 1_000, None, 0)
        .unwrap()
        .handle
        .unwrap();

    let state = store
        .status_at_domain(
            "mac-local",
            HostLeaseResourceClass::WholeMachine,
            "release",
            1_011,
        )
        .unwrap();
    assert!(state.active.is_none());
    assert!(state.recovered_stale_lease);
    assert_eq!(state.recovered, Some(prior));
}

#[test]
fn renew_and_release_are_domain_qualified() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut request = request("release-owner");
    request.domain = "release".to_string();
    let handle = store.try_acquire(request).unwrap().handle.unwrap();

    assert!(
        !store
            .renew_for_domain(
                &handle.host,
                handle.resource_class,
                "build",
                &handle.lease_id,
                120_000,
            )
            .unwrap()
            .renewed
    );
    assert!(
        !store
            .release_for_domain(
                &handle.host,
                handle.resource_class,
                "build",
                &handle.lease_id,
            )
            .unwrap()
            .released
    );
    assert_eq!(
        store
            .status_for_domain(&handle.host, handle.resource_class, "release")
            .unwrap()
            .active,
        Some(handle.clone())
    );
    assert!(
        store
            .release_for_domain(
                &handle.host,
                handle.resource_class,
                &handle.domain,
                &handle.lease_id,
            )
            .unwrap()
            .released
    );
}

#[test]
fn wrong_token_cannot_release_an_active_lease() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let first = store
        .try_acquire_at(request("codex-0"), 1_000, None, 0)
        .unwrap();
    let handle = first.handle.unwrap();
    assert!(!store.release(&handle.host, "wrong-token").unwrap().released);
    assert_eq!(
        store
            .status_at(&handle.host, HostLeaseResourceClass::WholeMachine, 1_001,)
            .unwrap()
            .active,
        Some(handle)
    );
}

#[test]
fn renew_requires_the_active_token() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let first = store.try_acquire(request("codex-0")).unwrap();
    let handle = first.handle.unwrap();

    assert!(
        !store
            .renew(&handle.host, "wrong-token", 120_000)
            .unwrap()
            .renewed
    );
    let renewed = store
        .renew(&handle.host, &handle.lease_id, 120_000)
        .unwrap();
    assert!(renewed.renewed);
    let renewed_handle = renewed.handle.unwrap();
    assert_eq!(renewed_handle.lease_id, handle.lease_id);
    assert!(renewed_handle.updated_at_ms >= handle.updated_at_ms);
    assert!(renewed_handle.expires_at_ms > Some(renewed_handle.updated_at_ms));
}

#[test]
fn metadata_replacement_requires_the_exact_active_authority() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut initial = request("release-owner");
    initial.domain = "release".to_string();
    initial.metadata = BTreeMap::from([
        ("discovery".to_string(), "pending".to_string()),
        ("obsolete".to_string(), "remove-me".to_string()),
    ]);
    let handle = store
        .try_acquire_at(initial, 1_000, None, 0)
        .unwrap()
        .handle
        .unwrap();
    let replacement = BTreeMap::from([
        ("discovery".to_string(), "complete".to_string()),
        ("revision".to_string(), "abc123".to_string()),
    ]);

    let wrong_token = store
        .update_metadata_at_domain(
            &handle.host,
            handle.resource_class,
            &handle.domain,
            "wrong-token",
            replacement.clone(),
            1_001,
        )
        .unwrap();
    assert!(!wrong_token.updated);
    assert!(wrong_token.handle.is_none());

    let wrong_domain = store
        .update_metadata_at_domain(
            &handle.host,
            handle.resource_class,
            "build",
            &handle.lease_id,
            replacement.clone(),
            1_002,
        )
        .unwrap();
    assert!(!wrong_domain.updated);
    assert!(wrong_domain.handle.is_none());

    let updated = store
        .update_metadata_at_domain(
            &handle.host,
            handle.resource_class,
            &handle.domain,
            &handle.lease_id,
            replacement.clone(),
            1_003,
        )
        .unwrap();
    assert!(updated.updated);
    let updated_handle = updated.handle.unwrap();
    assert_eq!(updated_handle.updated_at_ms, 1_003);
    assert_eq!(updated_handle.expires_at_ms, handle.expires_at_ms);
    assert_eq!(updated_handle.reason, handle.reason);
    assert_eq!(updated_handle.metadata, replacement);
    assert!(!updated_handle.metadata.contains_key("obsolete"));
    assert_eq!(
        store
            .status_at_domain(&handle.host, handle.resource_class, &handle.domain, 1_004,)
            .unwrap()
            .active,
        Some(updated_handle)
    );
}

#[test]
fn concurrent_metadata_replacements_never_merge_or_tear() {
    let temp = TempDir::new().unwrap();
    let store = Arc::new(store(&temp));
    let handle = store
        .try_acquire(request("metadata-owner"))
        .unwrap()
        .handle
        .unwrap();
    let first = BTreeMap::from([
        ("writer".to_string(), "first".to_string()),
        ("first-only".to_string(), "yes".to_string()),
    ]);
    let second = BTreeMap::from([
        ("writer".to_string(), "second".to_string()),
        ("second-only".to_string(), "yes".to_string()),
    ]);
    let barrier = Arc::new(Barrier::new(2));
    let workers = [first.clone(), second.clone()]
        .into_iter()
        .map(|metadata| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let handle = handle.clone();
            thread::spawn(move || {
                barrier.wait();
                store
                    .update_metadata_for_domain(
                        &handle.host,
                        handle.resource_class,
                        &handle.domain,
                        &handle.lease_id,
                        metadata,
                    )
                    .unwrap()
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        assert!(worker.join().unwrap().updated);
    }

    let active = store
        .status_for_domain(&handle.host, handle.resource_class, &handle.domain)
        .unwrap()
        .active
        .unwrap();
    assert!(active.metadata == first || active.metadata == second);
}

#[test]
fn non_expiring_lease_requires_a_live_owner_pid() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut missing = request("codex-0");
    missing.ttl_ms = None;
    let error = store.try_acquire_at(missing, 1_000, None, 0).unwrap_err();
    assert!(error.to_string().contains("requires owner_pid"));

    let mut live = request("codex-0");
    live.ttl_ms = None;
    live.owner_pid = Some(std::process::id());
    let acquired = store.try_acquire_at(live, 1_000, None, 0).unwrap();
    assert_eq!(acquired.status, HostLeaseAcquireStatus::Acquired);
    assert_eq!(acquired.handle.unwrap().expires_at_ms, None);
}

#[test]
fn unknown_process_liveness_preserves_the_active_lease() {
    let temp = TempDir::new().unwrap();
    let inspector = Arc::new(ScriptedProcessInspector::alive(42));
    let store = HostLeaseStore::for_root_with_inspector(
        temp.path(),
        Arc::clone(&inspector) as Arc<dyn ProcessInspector>,
    )
    .unwrap();
    let mut owner = request("codex-0");
    owner.ttl_ms = None;
    owner.owner_pid = Some(1234);
    store.try_acquire_at(owner, 1_000, None, 0).unwrap();

    inspector.set(ProcessObservation::Unknown);
    let deferred = store
        .try_acquire_at(request("codex-1"), 1_001, None, 0)
        .unwrap();
    assert_eq!(deferred.status, HostLeaseAcquireStatus::Deferred);
    assert!(!deferred.recovered_stale_lease);

    inspector.set(ProcessObservation::Dead);
    let recovered = store
        .try_acquire_at(request("codex-1"), 1_002, None, 0)
        .unwrap();
    assert_eq!(recovered.status, HostLeaseAcquireStatus::Acquired);
    assert!(recovered.recovered_stale_lease);
}

#[test]
fn non_expiring_owner_gets_a_bounded_liveness_wake() {
    let mut active = request("codex-0");
    active.ttl_ms = None;
    active.owner_pid = Some(1234);
    let handle = HostLeaseHandle {
        schema_version: SCHEMA_VERSION,
        host: active.host,
        resource_class: active.resource_class,
        domain: active.domain,
        execution_context: active.execution_context,
        lease_id: "lease-1".to_string(),
        owner: active.owner,
        priority_class: active.priority_class,
        acquired_at_ms: 1_000,
        updated_at_ms: 1_000,
        expires_at_ms: None,
        owner_pid: active.owner_pid,
        owner_process_identity: Some(42),
        reason: active.reason,
        metadata: active.metadata,
    };
    assert_eq!(next_lease_wake_at(&handle, 1_000, Some(60_000)), 6_000);
}

#[test]
fn registry_write_contention_returns_a_typed_defer_receipt() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut conn = store.connection(SQLITE_MUTATION_BUSY_TIMEOUT).unwrap();
    let _tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();

    let receipt = store
        .try_acquire_once_with_registry_timeout(request("codex-0"), None, None, Duration::ZERO)
        .unwrap();
    assert_eq!(receipt.status, HostLeaseAcquireStatus::Deferred);
    let defer = receipt.defer.unwrap();
    assert_eq!(defer.deferred_reason, HostLeaseDeferReason::RegistryBusy);
    assert!(defer.active.is_none());
}

#[test]
fn lease_database_uses_wal_journal_mode() {
    // WAL is what lets concurrent lease acquires/releases proceed without a
    // whole-database exclusive lock, so a briefly-contended writer no longer
    // surfaces "database is locked" under heavy parallel load. Assert the
    // persistent journal mode rather than the (platform-timing-dependent)
    // contention behavior so this holds on every OS.
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let conn = store.connection(SQLITE_MUTATION_BUSY_TIMEOUT).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
}

static BUSY_HANDLER_BARRIERS: OnceLock<(Barrier, Barrier)> = OnceLock::new();
static BUSY_HANDLER_OBSERVED: AtomicBool = AtomicBool::new(false);

fn release_registry_writer_after_busy_observed(_attempts: i32) -> bool {
    if !BUSY_HANDLER_OBSERVED.swap(true, Ordering::SeqCst) {
        let barriers = BUSY_HANDLER_BARRIERS.get().expect("busy barriers");
        barriers.0.wait();
        barriers.1.wait();
    }
    true
}

#[test]
fn immediate_acquire_waits_for_internal_registry_writer() {
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let mut blocker = store.connection(SQLITE_MUTATION_BUSY_TIMEOUT).unwrap();
    let transaction = blocker
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    BUSY_HANDLER_BARRIERS
        .set((Barrier::new(2), Barrier::new(2)))
        .expect("one deterministic busy-handler test");
    let store = store.with_busy_handler(release_registry_writer_after_busy_observed);

    let acquisition = thread::spawn(move || store.try_acquire(request("codex-0")).unwrap());
    let barriers = BUSY_HANDLER_BARRIERS.get().unwrap();
    barriers.0.wait();
    drop(transaction);
    barriers.1.wait();

    assert_eq!(
        acquisition.join().unwrap().status,
        HostLeaseAcquireStatus::Acquired
    );
}

#[test]
fn wait_rechecks_after_cross_thread_release_without_polling() {
    let temp = TempDir::new().unwrap();
    let store = Arc::new(store(&temp));
    let first = store.try_acquire(request("codex-0")).unwrap();
    let handle = first.handle.unwrap();
    let waiter = {
        let store = Arc::clone(&store);
        thread::spawn(move || {
            store
                .acquire_wait(request("codex-1"), Duration::from_secs(5))
                .unwrap()
        })
    };
    assert!(
        store
            .release(&handle.host, &handle.lease_id)
            .unwrap()
            .released
    );
    let receipt = waiter.join().unwrap();
    assert_eq!(receipt.status, HostLeaseAcquireStatus::Acquired);
}
