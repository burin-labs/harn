use super::*;

fn vm(root: &Path) -> Vm {
    let mut vm = Vm::new();
    register_checkpoint_builtins_at_state_root(&mut vm, root, "recovery");
    vm
}

#[tokio::test(flavor = "current_thread")]
async fn damaged_store_refuses_reads_and_mutations_until_explicit_recovery() {
    for bytes in ["{", "[]", "null", r#""synthetic-checkpoint-secret""#] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("checkpoints/recovery.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let mut vm = vm(root.path());
        for (name, args) in [
            ("checkpoint_get", vec![VmValue::string("spent")]),
            ("checkpoint_exists", vec![VmValue::string("spent")]),
            ("checkpoint_list", vec![]),
            (
                "checkpoint",
                vec![VmValue::string("spent"), VmValue::Int(0)],
            ),
            ("checkpoint_delete", vec![VmValue::string("spent")]),
        ] {
            let error = vm.call_named_builtin(name, args).await.unwrap_err();
            let message = error.to_string();
            assert!(
                message.contains("checkpoint decode error"),
                "{name} on {bytes:?}: {message}"
            );
            assert!(!message.contains("synthetic-checkpoint-secret"));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
        }
        vm.call_named_builtin("checkpoint_clear", vec![])
            .await
            .unwrap();
        assert!(!path.exists());
        vm.call_named_builtin(
            "checkpoint",
            vec![VmValue::string("spent"), VmValue::Int(7)],
        )
        .await
        .unwrap();
        let mut reloaded = self::vm(root.path());
        assert!(matches!(
            reloaded
                .call_named_builtin("checkpoint_get", vec![VmValue::string("spent")])
                .await
                .unwrap(),
            VmValue::Int(7)
        ));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn absent_store_is_empty_but_unreadable_store_is_not() {
    let root = tempfile::tempdir().unwrap();
    let mut vm = vm(root.path());
    assert!(matches!(
        vm.call_named_builtin("checkpoint_get", vec![VmValue::string("spent")])
            .await
            .unwrap(),
        VmValue::Nil
    ));
    let path = root.path().join("checkpoints/recovery.json");
    std::fs::create_dir_all(&path).unwrap();
    let mut reloaded = self::vm(root.path());
    assert!(reloaded
        .call_named_builtin("checkpoint_exists", vec![VmValue::string("spent")])
        .await
        .is_err());
    assert!(reloaded
        .call_named_builtin("checkpoint_clear", vec![])
        .await
        .is_err());
    assert!(reloaded
        .call_named_builtin("checkpoint_get", vec![VmValue::string("spent")])
        .await
        .is_err());
    assert!(path.is_dir());
}

#[tokio::test(flavor = "current_thread")]
async fn failed_save_invalidates_memory_and_reloads_persisted_value() {
    let root = tempfile::tempdir().unwrap();
    let mut vm = vm(root.path());
    let key = VmValue::string("spent");
    vm.call_named_builtin("checkpoint", vec![key.clone(), VmValue::Int(7)])
        .await
        .unwrap();
    let dir = root.path().join("checkpoints");
    let backup = root.path().join("saved");
    std::fs::rename(&dir, &backup).unwrap();
    std::fs::write(&dir, "blocks persistence").unwrap();
    assert!(vm
        .call_named_builtin("checkpoint", vec![key.clone(), VmValue::Int(0)])
        .await
        .is_err());
    assert!(vm
        .call_named_builtin("checkpoint_get", vec![key.clone()])
        .await
        .is_err());
    std::fs::remove_file(&dir).unwrap();
    std::fs::rename(&backup, &dir).unwrap();
    assert!(matches!(
        vm.call_named_builtin("checkpoint_get", vec![key])
            .await
            .unwrap(),
        VmValue::Int(7)
    ));
}
