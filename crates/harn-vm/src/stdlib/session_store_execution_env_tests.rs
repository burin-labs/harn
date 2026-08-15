use super::*;

async fn exercise_execution_env_store(
    label: &'static str,
    patch_mode: bool,
    root: PathBuf,
    fallback_root: PathBuf,
) -> PathBuf {
    crate::orchestration::scope_ambient(
        crate::orchestration::AmbientExecutionScope::default(),
        async move {
            let root_value = root.to_string_lossy().into_owned();
            let mut env = BTreeMap::new();
            if patch_mode {
                env.insert("HARN_TEST_INHERITED".to_string(), "kept".to_string());
            }
            env.insert(STORE_ROOT_ENV.to_string(), root_value.clone());
            crate::stdlib::process::set_thread_execution_context(Some(
                crate::orchestration::RunExecutionRecord {
                    cwd: Some(fallback_root.to_string_lossy().into_owned()),
                    project_root: Some(fallback_root.to_string_lossy().into_owned()),
                    env,
                    ..Default::default()
                },
            ));

            tokio::task::yield_now().await;
            assert_eq!(
                crate::stdlib::process::read_env_value(STORE_ROOT_ENV).as_deref(),
                Some(root_value.as_str()),
                "{label}: VM-visible environment must expose the child root"
            );
            assert_eq!(
                canonical_store_state_dir(None).unwrap(),
                SessionStoreDir::under_root(&root),
                "{label}: native store root must match the VM-visible environment"
            );

            let ctx = crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new());
            let database_path = session_store_database_path_impl(ctx.clone(), Vec::new())
                .await
                .unwrap();
            let database_path = PathBuf::from(database_path.display());
            assert_eq!(
                database_path,
                store_path(&SessionStoreDir::under_root(&root))
            );

            session_store_append_impl(
                ctx.clone(),
                vec![
                    VmValue::String(arcstr::ArcStr::from(format!("session.{label}"))),
                    json_to_vm_value(&json!({"child": label})),
                ],
            )
            .await
            .unwrap();
            tokio::task::yield_now().await;

            let events = session_store_events_impl(
                ctx,
                vec![VmValue::String(arcstr::ArcStr::from(format!(
                    "session.{label}"
                )))],
            )
            .await
            .unwrap();
            let VmValue::List(events) = events else {
                panic!("{label}: expected a list of stored events");
            };
            assert_eq!(events.len(), 1, "{label}: expected one isolated event");
            let child = events[0]
                .as_dict()
                .and_then(|event| event.get("payload"))
                .and_then(VmValue::as_dict)
                .and_then(|payload| payload.get("child"))
                .map(VmValue::display);
            assert_eq!(child.as_deref(), Some(label));
            database_path
        },
    )
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn execution_env_isolates_patch_replace_and_concurrent_stores() {
    let fallback = tempfile::tempdir().unwrap();
    let patch = tempfile::tempdir().unwrap();
    let replace = tempfile::tempdir().unwrap();
    let fallback_root = fallback.path().to_path_buf();
    let patch_root = patch.path().to_path_buf();
    let replace_root = replace.path().to_path_buf();

    let local = tokio::task::LocalSet::new();
    let (patch_path, replace_path) = local
        .run_until(async {
            let patch_child = tokio::task::spawn_local(exercise_execution_env_store(
                "patch",
                true,
                patch_root.clone(),
                fallback_root.clone(),
            ));
            let replace_child = tokio::task::spawn_local(exercise_execution_env_store(
                "replace",
                false,
                replace_root.clone(),
                fallback_root.clone(),
            ));
            (patch_child.await.unwrap(), replace_child.await.unwrap())
        })
        .await;

    assert_eq!(
        patch_path,
        store_path(&SessionStoreDir::under_root(&patch_root))
    );
    assert_eq!(
        replace_path,
        store_path(&SessionStoreDir::under_root(&replace_root))
    );
    assert!(patch_path.is_file());
    assert!(replace_path.is_file());
    assert_ne!(patch_path, replace_path);
    assert!(
        !store_path(&SessionStoreDir::under_root(&fallback_root)).exists(),
        "the inherited/global fallback store must remain untouched"
    );
}
