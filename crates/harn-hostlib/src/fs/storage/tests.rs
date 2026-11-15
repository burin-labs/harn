use super::*;
use crate::fs::{
    configure_session_root, read, read_dir, scope_conditional_replace_lock_root, set_mode,
    stage_write_or_none, staged_status,
};

#[test]
fn ordinary_v1_sessions_keep_their_bodies_and_upgrade_on_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let _locks = scope_conditional_replace_lock_root(root.join("locks"));
    let session = uuid::Uuid::new_v4().to_string();
    let path = root.join("document");
    fs::write(&path, b"before").unwrap();
    set_mode(&session, FsMode::Staged, Some(&root)).unwrap();
    stage_write_or_none("test", &path, b"legacy intent", false, true, Some(&session))
        .unwrap()
        .unwrap();
    let manifest = manifest_path(&root, &session);
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(value["version"], MANIFEST_VERSION);
    assert_eq!(value["atomic_groups"], serde_json::json!({}));
    value["version"] = serde_json::json!(1);
    value.as_object_mut().unwrap().remove("atomic_groups");
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    configure_session_root(&session, &root);
    assert_eq!(
        read(&path, Some(&session)).unwrap().unwrap(),
        b"legacy intent"
    );
    assert_eq!(staged_status(&session).unwrap().pending_writes.len(), 1);
    set_mode(&session, FsMode::Staged, None).unwrap();
    let upgraded: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(upgraded["version"], MANIFEST_VERSION);
    assert_eq!(upgraded["atomic_groups"], serde_json::json!({}));
    assert_eq!(
        read(&path, Some(&session)).unwrap().unwrap(),
        b"legacy intent"
    );
    assert_eq!(fs::read(path).unwrap(), b"before");
}

#[test]
fn incomplete_unknown_or_legacy_group_manifests_refuse_cached_queries_and_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let _locks = scope_conditional_replace_lock_root(root.join("locks"));
    let session = uuid::Uuid::new_v4().to_string();
    let path = root.join("document");
    fs::write(
        &path,
        b"physical disk must not hide an unavailable staged view",
    )
    .unwrap();
    set_mode(&session, FsMode::Immediate, Some(&root)).unwrap();
    let manifest = manifest_path(&root, &session);
    let original = fs::read(&manifest).unwrap();
    let valid: serde_json::Value = serde_json::from_slice(&original).unwrap();
    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("atomic_groups");
    let mut null = valid.clone();
    null["atomic_groups"] = serde_json::Value::Null;
    let mut unknown_version = valid.clone();
    unknown_version["version"] = serde_json::json!(MANIFEST_VERSION + 1);
    let mut legacy_group = valid.clone();
    legacy_group["version"] = serde_json::json!(1);
    let mut unknown_field = valid;
    unknown_field["future_required_policy"] = serde_json::json!(true);
    for invalid in [missing, null, unknown_version, legacy_group, unknown_field] {
        let bytes = serde_json::to_vec(&invalid).unwrap();
        fs::write(&manifest, &bytes).unwrap();
        assert!(read(&path, Some(&session))
            .expect("explicit refusal")
            .is_err());
        assert!(read_dir(&root, Some(&session))
            .expect("explicit refusal")
            .is_err());
        assert!(staged_status(&session).is_err());
        assert!(set_mode(&session, FsMode::Staged, None).is_err());
        assert_eq!(fs::read(&manifest).unwrap(), bytes);
        fs::write(&manifest, &original).unwrap();
        assert!(read(&path, Some(&session)).unwrap().is_ok());
        assert!(staged_status(&session).unwrap().pending_writes.is_empty());
    }
}

#[cfg(unix)]
#[test]
fn immediate_reads_do_not_create_journals_in_read_only_workspaces() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let path = root.join("document");
    fs::write(&path, b"readable source").unwrap();
    let _locks = scope_conditional_replace_lock_root(directory.path().join("host-state"));
    fs::set_permissions(&root, fs::Permissions::from_mode(0o555)).unwrap();
    let result = std::panic::catch_unwind(|| {
        assert!(fs::write(root.join("write-control"), b"must refuse").is_err());
        let session = uuid::Uuid::new_v4().to_string();
        configure_session_root(&session, &root);
        assert_eq!(
            read(&path, Some(&session)).unwrap().unwrap(),
            b"readable source"
        );
        let entries = read_dir(&root, Some(&session)).unwrap().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "document");
        assert!(staged_status(&session).unwrap().pending_writes.is_empty());
        assert!(!root.join(".harn").exists());
        assert_eq!(
            fs::read_dir(directory.path().join("host-state"))
                .unwrap()
                .count(),
            1
        );
    });
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    result.unwrap();
}
