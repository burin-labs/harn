use super::*;

#[tokio::test]
async fn module_export_call_commits_initialization_once_and_preserves_caller_scope() {
    let root = tempfile::tempdir().unwrap();
    let module = root.path().join("callee.harn");
    std::fs::write(
        &module,
        "const initialized = initialize()\npub fn answer() { return initialized }",
    )
    .unwrap();
    let mut vm = crate::vm::test_runtime::new_vm();
    vm.env
        .define("caller_value", VmValue::Int(7), false)
        .unwrap();
    let caller_dir = root.path().join("caller");
    vm.set_source_dir(&caller_dir);
    let caller_import = root.path().join("already-imported.harn");
    vm.imported_paths.push(caller_import.clone());
    vm.output.push_str("caller\n");
    let initializations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = initializations.clone();
    vm.register_builtin("initialize", move |_, output| {
        observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        output.push_str("initialized\n");
        Ok(VmValue::Int(41))
    });

    for _ in 0..2 {
        let value = vm
            .call_module_export(module.to_str().unwrap(), "answer", "test", &[])
            .await
            .unwrap();
        assert!(matches!(value, VmValue::Int(41)));
        assert!(matches!(vm.env.get("caller_value"), Some(VmValue::Int(7))));
        assert_eq!(vm.source_dir(), Some(caller_dir.as_path()));
        assert!(vm.imported_paths.contains(&caller_import));
    }
    assert_eq!(initializations.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(vm.output(), "caller\ninitialized\n");
}

#[tokio::test]
async fn cancelling_module_export_initialization_preserves_caller_and_allows_retry() {
    let root = tempfile::tempdir().unwrap();
    let module = root.path().join("callee.harn");
    std::fs::write(
        &module,
        "const initialized = initialize()\npub fn answer() { return initialized }",
    )
    .unwrap();
    let mut vm = crate::vm::test_runtime::new_vm();
    vm.env
        .define("caller_value", VmValue::Int(7), false)
        .unwrap();
    vm.set_source_dir(root.path());
    vm.output.push_str("caller\n");
    let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
    vm.register_async_builtin("initialize", move |_, _| {
        let entered = entered_tx.clone();
        async move {
            entered.send(()).unwrap();
            std::future::pending::<Result<VmValue, VmError>>().await
        }
    });
    {
        let load = vm.call_module_export(module.to_str().unwrap(), "answer", "test", &[]);
        tokio::pin!(load);
        tokio::select! {
            entered = entered_rx.recv() => assert_eq!(entered, Some(())),
            result = &mut load => panic!("initializer did not suspend: {result:?}"),
        }
    }
    assert!(matches!(vm.env.get("caller_value"), Some(VmValue::Int(7))));
    assert_eq!(vm.source_dir(), Some(root.path()));
    assert!(vm.module_cache.is_empty());
    assert_eq!(vm.output(), "caller\n");

    vm.register_async_builtin("initialize", |_, _| async { Ok(VmValue::Int(42)) });
    let value = vm
        .call_module_export(module.to_str().unwrap(), "answer", "test", &[])
        .await
        .unwrap();
    assert!(matches!(value, VmValue::Int(42)));
    assert!(matches!(vm.env.get("caller_value"), Some(VmValue::Int(7))));
}
