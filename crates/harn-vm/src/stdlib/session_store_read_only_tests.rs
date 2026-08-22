use super::*;

fn durable_inventory(root: &Path) -> Vec<(PathBuf, u64, String)> {
    fn visit(root: &Path, path: &Path, out: &mut Vec<(PathBuf, u64, String)>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let child = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &child, out);
            } else {
                let bytes = fs::read(&child).unwrap();
                out.push((
                    child.strip_prefix(root).unwrap().to_path_buf(),
                    bytes.len() as u64,
                    hex::encode(Sha256::digest(&bytes)),
                ));
            }
        }
    }

    let mut out = Vec::new();
    if root.is_dir() {
        visit(root, root, &mut out);
    }
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}

fn root_options(root: &Path) -> VmValue {
    json_to_vm_value(&json!({"root": root.to_string_lossy()}))
}

#[tokio::test(flavor = "current_thread")]
async fn missing_and_malformed_read_surfaces_do_not_materialize_state() {
    let dir = tempfile::tempdir().unwrap();
    let options = root_options(dir.path());
    let ctx = || crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new());

    assert!(matches!(
        session_store_list_impl(ctx(), vec![options.clone()]).await,
        Ok(VmValue::List(items)) if items.is_empty()
    ));
    assert!(matches!(
        session_store_events_impl(
            ctx(),
            vec![VmValue::String(arcstr::ArcStr::from("missing")), options.clone()],
        )
        .await,
        Ok(VmValue::List(items)) if items.is_empty()
    ));
    session_store_verify_impl(
        ctx(),
        vec![
            VmValue::String(arcstr::ArcStr::from("missing")),
            options.clone(),
        ],
    )
    .await
    .unwrap();
    let search_options = json_to_vm_value(&json!({
        "root": dir.path().to_string_lossy(),
        "project_scope": "missing-project",
    }));
    session_store_search_impl(
        ctx(),
        vec![
            VmValue::String(arcstr::ArcStr::from("anything")),
            search_options,
        ],
    )
    .await
    .unwrap();
    assert!(durable_inventory(dir.path()).is_empty());

    let malformed = json_to_vm_value(&json!({
        "root": dir.path().to_string_lossy(),
        "status": "not-a-status",
    }));
    assert!(session_store_list_impl(ctx(), vec![malformed])
        .await
        .is_err());
    assert!(durable_inventory(dir.path()).is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn canonical_read_surfaces_preserve_the_full_durable_inventory() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = SessionStoreDir::under_root(dir.path());
    let store = open_store(&state_dir).unwrap();
    ensure_session(&store, &state_dir, "inspect", true, None)
        .await
        .unwrap();
    store
        .append(
            "inspect",
            AppendEvent::new(
                SessionEventKind::Custom {
                    custom_type: "inspection".to_string(),
                },
                json!({
                    "message": "inspection evidence",
                    "token": "sk-proj-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                }),
            ),
        )
        .await
        .unwrap();
    drop(store);

    let mut lock_path = store_path(&state_dir).as_os_str().to_os_string();
    lock_path.push(".harn-init.lock");
    let lock_path = PathBuf::from(lock_path);
    fs::remove_file(&lock_path).unwrap();
    let before = durable_inventory(dir.path());
    let raw_before = fs::read(store_path(&state_dir)).unwrap();
    assert!(!String::from_utf8_lossy(&raw_before).contains("sk-proj-"));

    let options = root_options(dir.path());
    let ctx = || crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new());
    session_store_list_impl(ctx(), vec![options.clone()])
        .await
        .unwrap();
    session_store_events_impl(
        ctx(),
        vec![
            VmValue::String(arcstr::ArcStr::from("inspect")),
            options.clone(),
        ],
    )
    .await
    .unwrap();
    session_store_verify_impl(
        ctx(),
        vec![
            VmValue::String(arcstr::ArcStr::from("inspect")),
            options.clone(),
        ],
    )
    .await
    .unwrap();
    let search_options = json_to_vm_value(&json!({
        "root": dir.path().to_string_lossy(),
        "project_scope": "inspect-project",
    }));
    session_store_search_impl(
        ctx(),
        vec![
            VmValue::String(arcstr::ArcStr::from("inspection")),
            search_options,
        ],
    )
    .await
    .unwrap();

    assert_eq!(durable_inventory(dir.path()), before);
    assert!(!lock_path.exists());
}
