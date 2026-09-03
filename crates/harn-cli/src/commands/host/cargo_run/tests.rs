//! Unit tests for supervised Cargo execution behind a machine-resource lease.

use super::{
    finalize_run, format_lease_wait, format_worker_binary, path_argument, terminal_projection,
    wait_for_cargo_workload, BinaryWitness, WorkerCompletion, EX_TEMPFAIL, EX_TIMEOUT,
};
use harn_hostlib::process::{
    EnvMode, MockProcessConfig, MockSpawner, OutputCapture, OwnerDeathPolicy, ProcessSpawner,
    SpawnSpec,
};
use std::collections::BTreeMap;
use std::path::Path;
use tempfile::TempDir;

#[test]
fn pending_worker_exit_is_persisted_as_start_failed_not_cargo_failure() {
    let temp = TempDir::new().unwrap();
    let store = harn_hostlib::HostLeaseStore::for_root(temp.path()).unwrap();
    let run = store
        .begin_run(
            "waiting-lane",
            harn_hostlib::HostLeasePriorityClass::Measurement,
            harn_hostlib::HostLeaseResourceKey {
                machine: "fixture".to_string(),
                resource_class: harn_hostlib::HostLeaseResourceClass::RustHeavy,
                domain: harn_hostlib::DEFAULT_HOST_LEASE_DOMAIN.to_string(),
            },
            harn_hostlib::HostLeaseExecutionContext::cargo(
                Path::new("/workspace/project"),
                Path::new("/tmp/target"),
                None,
            ),
            5_000,
        )
        .unwrap();

    let binary = temp.path().join("worker-binary");
    std::fs::write(&binary, b"original").unwrap();
    let witness = BinaryWitness::observe(binary);

    let exit = finalize_run(
        &store,
        &run.run_id,
        WorkerCompletion::Exited(harn_hostlib::process::ExitStatus::from_code(101)),
        &witness,
    )
    .unwrap();

    assert_eq!(exit, EX_TEMPFAIL);
    // The durable half of the #7829 falsifier: the receipt itself must
    // carry the worker's status, so a later reader can tell an external
    // kill from an early return without the process still being alive.
    let status = store.load_run(&run.run_id).unwrap().status;
    let harn_hostlib::HostLeaseRunState::StartFailed {
        error: harn_hostlib::HostLeaseRunStartFailure::WorkerExitedBeforeAcquire,
        worker_exit,
        ..
    } = status
    else {
        panic!("pending worker exit was not persisted as a start failure: {status:?}");
    };
    assert_eq!(
        worker_exit,
        Some(harn_hostlib::HostLeaseProcessExit {
            code: Some(101),
            signal: None,
        }),
        "the receipt discarded the only evidence of why the worker is gone"
    );
}

/// A concurrent rebuild of a shared `target/debug` is one of the causes
/// the old single label hid. The supervisor can see it without the worker.
#[test]
fn a_binary_swapped_under_a_running_worker_is_named_in_the_receipt() {
    let temp = TempDir::new().unwrap();
    let binary = temp.path().join("worker-binary");
    std::fs::write(&binary, b"original").unwrap();
    let witness = BinaryWitness::observe(binary.clone());
    assert_eq!(
        witness.replaced_since_spawn(),
        Some(false),
        "an untouched binary must not read as replaced"
    );

    std::fs::write(&binary, b"a rebuild landed here mid-run").unwrap();
    assert_eq!(
        witness.replaced_since_spawn(),
        Some(true),
        "a rebuilt binary was not detected under the running worker"
    );

    // An unreadable path proves nothing, and must never read as verified.
    let missing = BinaryWitness::observe(temp.path().join("never-existed"));
    assert_eq!(missing.replaced_since_spawn(), None);
    assert_eq!(format_worker_binary(None), "unverified");
    assert_eq!(format_worker_binary(Some(false)), "unchanged");
    assert_eq!(format_worker_binary(Some(true)), "replaced-during-run");
}

#[test]
fn start_failed_uses_a_reserved_supervisor_status_and_stable_diagnostic() {
    let projection = terminal_projection(
        &harn_hostlib::HostLeaseRunState::StartFailed {
            observed_at_ms: 1,
            error: harn_hostlib::HostLeaseRunStartFailure::WorkerExitedBeforeAcquire,
            worker_exit: None,
            worker_binary_replaced: None,
        },
        101,
        None,
    )
    .unwrap();

    assert_eq!(projection.exit_code, EX_TEMPFAIL);
    assert_eq!(
            projection.diagnostic.as_deref(),
            Some(
                "error: Cargo workload did not start (state=start-failed error=worker-exited-before-acquire worker_exit=unrecorded worker_binary=unverified waited=unrecorded limit=unrecorded queue_position=unrecorded)"
            )
        );
}

/// The falsifier for #7829: a signalled worker and one that returned early
/// must not produce the same receipt or the same operator-facing line.
#[test]
fn a_signalled_pre_acquire_worker_is_distinguishable_from_an_early_return() {
    let signalled = terminal_projection(
        &harn_hostlib::HostLeaseRunState::StartFailed {
            observed_at_ms: 1,
            error: harn_hostlib::HostLeaseRunStartFailure::WorkerExitedBeforeAcquire,
            worker_exit: Some(harn_hostlib::HostLeaseProcessExit {
                code: None,
                signal: Some(9),
            }),
            worker_binary_replaced: None,
        },
        0,
        None,
    )
    .unwrap();
    let returned = terminal_projection(
        &harn_hostlib::HostLeaseRunState::StartFailed {
            observed_at_ms: 1,
            error: harn_hostlib::HostLeaseRunStartFailure::WorkerExitedBeforeAcquire,
            worker_exit: Some(harn_hostlib::HostLeaseProcessExit {
                code: Some(75),
                signal: None,
            }),
            worker_binary_replaced: None,
        },
        0,
        None,
    )
    .unwrap();

    let signalled = signalled.diagnostic.expect("signalled death is diagnosed");
    let returned = returned.diagnostic.expect("early return is diagnosed");
    assert!(
        signalled.contains("worker_exit=signal:9"),
        "signalled worker did not name its signal: {signalled}"
    );
    assert!(
        returned.contains("worker_exit=code:75"),
        "returning worker did not name its code: {returned}"
    );
    assert_ne!(
        signalled, returned,
        "two different pre-acquire deaths still read identically"
    );
}

/// An expired wait is a lease outcome, not a silent one. Before #7829 a
/// deferred run printed only a receipt path, so starvation and success
/// were the same terminal output.
#[test]
fn an_expired_wait_names_itself_instead_of_printing_nothing() {
    let projection = terminal_projection(
        &harn_hostlib::HostLeaseRunState::Deferred {
            observed_at_ms: 2,
            waited_ms: 3_600_000,
        },
        0,
        None,
    )
    .unwrap();

    assert_eq!(projection.exit_code, EX_TEMPFAIL);
    let diagnostic = projection.diagnostic.expect("an expired wait is diagnosed");
    assert!(
        diagnostic.contains("state=deferred"),
        "expired wait did not name its terminal state: {diagnostic}"
    );
}

#[test]
fn completed_workload_preserves_the_real_cargo_status() {
    let projection = terminal_projection(
        &harn_hostlib::HostLeaseRunState::Completed {
            lease_id: "lease".to_string(),
            acquire_wait_ms: 0,
            hold_ms: 1,
            worker_pid: 7,
            exit: harn_hostlib::HostLeaseProcessExit {
                code: Some(101),
                signal: None,
            },
            release: harn_hostlib::HostLeaseRunReleaseOutcome::Released,
            finished_at_ms: 2,
        },
        101,
        None,
    )
    .unwrap();

    assert_eq!(projection.exit_code, 101);
    assert!(projection.diagnostic.is_none());
}

#[test]
fn wait_progress_projects_holder_queue_and_elapsed_time() {
    let receipt = harn_hostlib::HostLeaseAcquireReceipt {
        schema_version: 4,
        status: harn_hostlib::HostLeaseAcquireStatus::Deferred,
        observed_at_ms: 31_000,
        waited_ms: 31_000,
        handle: None,
        defer: Some(harn_hostlib::HostLeaseDeferReceipt {
            host: "fixture".to_string(),
            resource_class: harn_hostlib::HostLeaseResourceClass::RustHeavy,
            domain: harn_hostlib::DEFAULT_HOST_LEASE_DOMAIN.to_string(),
            deferred_reason: harn_hostlib::HostLeaseDeferReason::Contended,
            observed_at_ms: 31_000,
            next_wake_at_ms: None,
            deadline_at_ms: Some(90_000),
            active: Some(harn_hostlib::HostLeaseHandle {
                schema_version: 4,
                host: "fixture".to_string(),
                resource_class: harn_hostlib::HostLeaseResourceClass::RustHeavy,
                domain: harn_hostlib::DEFAULT_HOST_LEASE_DOMAIN.to_string(),
                execution_context: None,
                lease_id: "opaque".to_string(),
                owner: "compile-lane".to_string(),
                priority_class: harn_hostlib::HostLeasePriorityClass::Measurement,
                acquired_at_ms: 0,
                updated_at_ms: 0,
                expires_at_ms: None,
                owner_pid: None,
                owner_process_identity: None,
                reason: None,
                metadata: BTreeMap::new(),
            }),
        }),
        recovered_stale_lease: false,
        recovered: None,
        queue: Some(harn_hostlib::HostLeaseQueueEvidence {
            waiter_id: "waiter".to_string(),
            requested_at_ms: 0,
            position: 2,
            predecessor_waiter_id: Some("first".to_string()),
        }),
    };

    assert_eq!(
        format_lease_wait(&receipt),
        "Waiting for rust-heavy lease held by compile-lane (queue position 2, elapsed 31.0s)"
    );

    let mut queued = receipt;
    let queued_defer = queued.defer.as_mut().unwrap();
    queued_defer.deferred_reason = harn_hostlib::HostLeaseDeferReason::Queued;
    queued_defer.active = None;
    assert_eq!(
        format_lease_wait(&queued),
        "Waiting for rust-heavy lease admission (queue position 2, elapsed 31.0s)"
    );

    let mut registry_busy = queued;
    registry_busy.defer.as_mut().unwrap().deferred_reason =
        harn_hostlib::HostLeaseDeferReason::RegistryBusy;
    assert_eq!(
        format_lease_wait(&registry_busy),
        "Waiting for rust-heavy lease registry (queue position 2, elapsed 31.0s)"
    );
}

#[test]
fn child_process_paths_strip_windows_drive_verbatim_prefixes() {
    assert_eq!(
        path_argument(Path::new(r"\\?\E:\target\debug")).unwrap(),
        r"E:\target\debug"
    );
}

#[test]
fn child_process_paths_strip_windows_unc_verbatim_prefixes() {
    assert_eq!(
        path_argument(Path::new(r"\\?\UNC\server\share\target")).unwrap(),
        r"\\server\share\target"
    );
}

#[test]
fn child_process_paths_preserve_non_verbatim_spelling() {
    for path in [r"E:\target\debug", r"\\server\share\target", "/tmp/target"] {
        assert_eq!(path_argument(Path::new(path)).unwrap(), path);
    }
}

#[test]
fn cargo_workload_timeout_starts_at_the_worker_boundary() {
    let spawner = MockSpawner::new();
    spawner.enqueue(MockProcessConfig {
        force_timeout: true,
        ..MockProcessConfig::default()
    });
    let mut child = spawner
        .spawn(SpawnSpec {
            builtin: "test",
            program: "cargo".to_string(),
            args: vec!["check".to_string()],
            cwd: None,
            env: BTreeMap::new(),
            env_remove: Vec::new(),
            env_mode: EnvMode::InheritClean,
            use_stdin: true,
            configure_process_group: false,
            owner_death: OwnerDeathPolicy::None,
            output_capture: OutputCapture::Inherit,
        })
        .unwrap();

    assert_eq!(
        wait_for_cargo_workload(child.as_mut(), Some(600_000)),
        EX_TIMEOUT
    );
}
