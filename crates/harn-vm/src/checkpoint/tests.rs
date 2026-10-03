use super::*;

#[test]
fn independent_checkpoint_owners_preserve_other_keys_and_observe_updates() {
    let root = tempfile::tempdir().unwrap();
    let mut first = CheckpointState::at_state_root(root.path(), "shared");
    let mut second = CheckpointState::at_state_root(root.path(), "shared");
    assert!(matches!(first.get("first").unwrap(), VmValue::Nil));
    assert!(matches!(second.get("second").unwrap(), VmValue::Nil));

    first.set("first".into(), serde_json::json!(1)).unwrap();
    second.set("second".into(), serde_json::json!(2)).unwrap();
    let mut fresh = CheckpointState::at_state_root(root.path(), "shared");
    assert!(
        matches!(fresh.get("first").unwrap(), VmValue::Int(1)),
        "a stale owner must preserve another owner's committed key"
    );
    assert!(matches!(first.get("second").unwrap(), VmValue::Int(2)));
    assert!(first.exists("second").unwrap());
    assert_eq!(first.list().unwrap(), ["first", "second"]);

    second.delete("first").unwrap();
    assert!(matches!(first.get("first").unwrap(), VmValue::Nil));
    second.clear().unwrap();
    assert!(first.list().unwrap().is_empty());
}

#[test]
fn concurrent_insert_retains_one_candidate_and_reports_one_winner() {
    let root = tempfile::tempdir().unwrap();
    let ready = std::sync::Arc::new(std::sync::Barrier::new(8));
    let receipts = crate::runtime_stack::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|candidate| {
                let ready = ready.clone();
                let root = root.path();
                scope.spawn(move || {
                    let mut state = CheckpointState::at_state_root(root, "shared");
                    assert!(matches!(state.get("candidate").unwrap(), VmValue::Nil));
                    ready.wait();
                    vm_to_json(
                        &state
                            .insert("candidate".into(), serde_json::json!(candidate))
                            .unwrap(),
                    )
                    .unwrap()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(receipts.len(), 8);
    assert_eq!(
        receipts
            .iter()
            .filter(|receipt| receipt["inserted"] == true)
            .count(),
        1
    );
    let retained = &receipts[0]["value"];
    assert!(retained
        .as_i64()
        .is_some_and(|value| (0..8).contains(&value)));
    assert!(receipts.iter().all(|receipt| &receipt["value"] == retained));
}

#[test]
fn insert_preserves_a_retained_null_value() {
    let root = tempfile::tempdir().unwrap();
    let mut first = CheckpointState::at_state_root(root.path(), "shared");
    assert_eq!(
        vm_to_json(
            &first
                .insert("candidate".into(), serde_json::Value::Null)
                .unwrap()
        )
        .unwrap(),
        serde_json::json!({"inserted": true, "value": null})
    );
    let mut second = CheckpointState::at_state_root(root.path(), "shared");
    assert_eq!(
        vm_to_json(
            &second
                .insert("candidate".into(), serde_json::json!(7))
                .unwrap()
        )
        .unwrap(),
        serde_json::json!({"inserted": false, "value": null})
    );
}

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
            (
                "checkpoint_insert",
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

/// Windows reports a non-directory ancestor as "not found", so a blocked
/// store must be refused by the ancestor walk rather than by the OS error
/// kind. Exercise the walk directly so every platform runs it.
#[test]
fn a_store_beneath_a_file_is_unreachable_but_a_missing_one_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let missing = CheckpointState::at_state_root(&root.path().join("never/created"), "recovery");
    assert_eq!(missing.confirm_absent(), Ok(()));

    std::fs::write(root.path().join("checkpoints"), "blocks persistence").unwrap();
    let blocked = CheckpointState::at_state_root(root.path(), "recovery");
    let error = blocked.confirm_absent().unwrap_err();
    assert!(error.contains("is not a directory"), "{error}");
}
