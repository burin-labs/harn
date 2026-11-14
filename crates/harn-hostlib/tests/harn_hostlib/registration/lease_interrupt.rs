use std::sync::atomic::{AtomicBool, Ordering};

use notify::Watcher;

use super::*;

#[test]
fn lease_acquire_yields_to_cancel_on_the_session_thread_and_retires_its_waiter() {
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
        cancel.store(true, Ordering::SeqCst);
        assert!(execution.await.is_err(), "cancelled acquisition must not return ownership");
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
