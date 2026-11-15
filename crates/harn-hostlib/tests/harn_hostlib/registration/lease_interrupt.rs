use std::sync::atomic::{AtomicBool, Ordering};

use notify::Watcher;

use super::*;

#[test]
fn lease_acquire_yields_to_cancel_on_the_session_thread_and_retires_its_waiter() {
    assert_cancellable_wait(false);
}

#[test]
fn lease_cancel_finishes_while_a_registry_writer_blocks_cleanup() {
    assert_cancellable_wait(true);
}

#[test]
fn delivered_lease_release_yields_to_vm_cancellation_while_cleanup_is_blocked() {
    let root = TempDir::new().unwrap();
    let _env = HostLeaseRootGuard::set(root.path());
    let store = HostLeaseStore::for_root(root.path()).unwrap();
    let (events, mut changes) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.expect("release notification").kind.is_modify() {
            let _ = events.send(());
        }
    })
    .unwrap();
    watcher
        .watch(
            &root.path().join("host-leases.wake"),
            notify::RecursiveMode::NonRecursive,
        )
        .unwrap();
    let (started, mut release_started) = tokio::sync::mpsc::unbounded_channel();
    let (unlock, unlocked) = std::sync::mpsc::channel();
    let unlocked = Arc::new(Mutex::new(Some(unlocked)));
    let db = root.path().join("host-leases.sqlite");
    let source = r#"
pipeline default(harness: Harness) {
  const acquired = harness.host_lease.acquire({
    host: "release-host", resource_class: "rust-heavy", domain: "release-domain",
    owner: "delivered", priority_class: "measurement", wait_slice_ms: 0, metadata: {},
  })
  const observed = observe_release(acquired.guard)
  return harness.host_lease.release({guard: observed})
}
"#;
    let mut lexer = Lexer::new(source);
    let mut parser = Parser::new(lexer.tokenize().unwrap());
    let chunk = Compiler::new().compile(&parser.parse().unwrap()).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let rt = harn_clock::test_support::current_thread_runtime();
    rt.block_on(harn_clock::test_support::with_local_tasks(async {
        let mut vm = harn_vm::new_vm();
        register_vm_stdlib(&mut vm);
        let _ = harn_hostlib::install_default(&mut vm);
        vm.set_harness(harn_vm::Harness::real());
        vm.install_cancel_token(Arc::clone(&cancel));
        vm.register_builtin("observe_release", move |args, _| {
            let Some(VmValue::ResourceGuard(guard)) = args.first() else {
                panic!("the witness requires the actual delivered native guard");
            };
            let guard = Arc::clone(guard);
            let unlocked = unlocked.lock().unwrap().take().expect("one release witness");
            let db = db.clone();
            let started = started.clone();
            Ok(VmValue::resource_guard(harn_vm::VmResourceGuardHandle::new("release-witness", move || {
                let (locked, observed) = std::sync::mpsc::channel();
                let writer = harn_parser::runtime_stack::spawn(move || {
                    let mut connection = rusqlite::Connection::open(db).unwrap();
                    let transaction = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap();
                    let active: i64 = transaction.query_row("SELECT COUNT(*) FROM host_leases WHERE expires_at_ms IS NULL", [], |row| row.get(0)).unwrap();
                    locked.send(active).unwrap();
                    if unlocked.recv().is_ok() { transaction.commit().unwrap(); }
                });
                let active = harn_clock::test_support::recv_within("writer reached delivered lease", &observed);
                started.send(active).unwrap();
                let result = guard.release().map_err(|error| format!("{error:?}"));
                writer.join().unwrap();
                result
            })))
        });
        let execution = vm.execute(&chunk);
        tokio::pin!(execution);
        tokio::select! {
            result = &mut execution => panic!("release returned before its cancellation witness: {result:?}"),
            active = release_started.recv() => assert_eq!(active, Some(1), "the real release must reach a held writer and delivered TTL:nil lease"),
        }
        cancel.store(true, Ordering::SeqCst);
        let error = execution.await.expect_err("release must yield to cancellation on its VM thread");
        assert_eq!(harn_vm::value::error_to_category(&error), harn_vm::value::ErrorCategory::Cancelled);
        // No writer release was sent until the caller's cancellation completed.
        unlock.send(()).unwrap();
        loop {
            let state = store.status_for_domain("release-host", HostLeaseResourceClass::RustHeavy, "release-domain").unwrap();
            if state.active.is_none() { assert!(state.pending.is_empty()); break; }
            changes.recv().await.expect("physical release event");
        }
    }));
}

fn assert_cancellable_wait(block_cleanup: bool) {
    let root = TempDir::new().unwrap();
    let _env = HostLeaseRootGuard::set(root.path());
    let store = HostLeaseStore::for_root(root.path()).unwrap();
    let holder = store
        .try_acquire(HostLeaseRequest {
            host: "cancel-host".to_string(),
            resource_class: HostLeaseResourceClass::RustHeavy,
            domain: "cancel-domain".to_string(),
            execution_context: None,
            owner: "holder".to_string(),
            priority_class: HostLeasePriorityClass::Measurement,
            ttl_ms: Some(60_000),
            owner_pid: None,
            reason: None,
            metadata: BTreeMap::new(),
        })
        .unwrap()
        .handle
        .unwrap();
    let (events, mut changes) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let event = event.expect("lease registry watch");
        if event.kind.is_modify() || event.kind.is_create() {
            let _ = events.send(());
        }
    })
    .unwrap();
    watcher
        .watch(root.path(), notify::RecursiveMode::NonRecursive)
        .unwrap();
    let observed = || {
        store
            .status_for_domain(
                "cancel-host",
                HostLeaseResourceClass::RustHeavy,
                "cancel-domain",
            )
            .unwrap()
    };
    let source = r#"
pipeline default(harness: Harness) {
  return harness.host_lease.acquire({
    host: "cancel-host", resource_class: "rust-heavy", domain: "cancel-domain",
    owner: "cancelled", priority_class: "measurement", wait_slice_ms: 5000, metadata: {},
  })
}
"#;
    let mut lexer = Lexer::new(source);
    let mut parser = Parser::new(lexer.tokenize().unwrap());
    let chunk = Compiler::new().compile(&parser.parse().unwrap()).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let rt = harn_clock::test_support::current_thread_runtime();
    rt.block_on(harn_clock::test_support::with_local_tasks(async {
        let mut vm = harn_vm::new_vm();
        register_vm_stdlib(&mut vm);
        let _ = harn_hostlib::install_default(&mut vm);
        vm.set_harness(harn_vm::Harness::real());
        vm.install_cancel_token(Arc::clone(&cancel));
        let execution = vm.execute(&chunk);
        tokio::pin!(execution);
        loop {
            tokio::select! {
                result = &mut execution => panic!("the wait returned before cancellation could run on its thread: {result:?}"),
                event = changes.recv() => {
                    event.expect("registry event stream");
                    if !observed().pending.is_empty() {
                        break;
                    }
                }
            }
        }
        let mut writer = block_cleanup.then(|| rusqlite::Connection::open(root.path().join("host-leases.sqlite")).unwrap());
        let transaction = writer.as_mut().map(|connection| {
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).unwrap()
        });
        cancel.store(true, Ordering::SeqCst);
        if let Some(transaction) = transaction {
            let error = execution.await.expect_err("cancelled acquisition returned ownership");
            assert_eq!(harn_vm::value::error_to_category(&error), harn_vm::value::ErrorCategory::Cancelled, "expected runtime cancellation, got {error:?}");
            let pending: i64 = transaction.query_row("SELECT COUNT(*) FROM host_lease_waiters", [], |row| row.get(0)).unwrap();
            assert_eq!(pending, 1, "the writer must still exclude cleanup");
            transaction.commit().unwrap();
        } else {
            assert!(execution.await.is_err(), "cancelled acquisition must not return ownership");
        }
        loop {
            let state = observed();
            assert_eq!(state.active.unwrap().lease_id, holder.lease_id);
            if state.pending.is_empty() {
                break;
            }
            changes.recv().await.expect("waiter retirement event");
        }
    }));
    assert!(
        store
            .release_for_domain(
                &holder.host,
                holder.resource_class,
                &holder.domain,
                &holder.lease_id
            )
            .unwrap()
            .released
    );
}
