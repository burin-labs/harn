use super::*;

#[test]
fn aliases_are_exclusive_idempotent_and_never_replace_host_entries() {
    let base = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let suffix = prepare_in(base.path(), target.path()).unwrap();
    let alias = base.path().join(&suffix);
    assert_eq!(std::fs::read_link(&alias).unwrap(), target.path());
    assert_eq!(prepare_in(base.path(), target.path()).unwrap(), suffix);

    std::fs::remove_file(&alias).unwrap();
    std::fs::write(&alias, "host-owned").unwrap();
    assert!(prepare_in(base.path(), target.path()).is_err());
    assert_eq!(std::fs::read_to_string(&alias).unwrap(), "host-owned");

    std::fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(base.path(), &alias).unwrap();
    assert!(prepare_in(base.path(), target.path()).is_err());
    assert_eq!(std::fs::read_link(&alias).unwrap(), base.path());
}

#[test]
fn inherited_native_suffix_does_not_nest_the_next_workspace_alias() {
    let base = Path::new("/private/var/folders/user/T");
    assert_eq!(unsuffixed_base(base, None).unwrap(), base);
    assert_eq!(
        unsuffixed_base(&base.join("harn-parent"), Some(OsStr::new("harn-parent"))).unwrap(),
        base
    );
    assert!(unsuffixed_base(base, Some(OsStr::new("mismatch"))).is_err());
    assert!(unsuffixed_base(Path::new("relative"), None).is_err());
}

#[test]
fn workspace_temp_symlinks_cannot_select_host_storage() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let policy = CapabilityPolicy {
        sandbox_profile: crate::orchestration::SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().display().to_string()],
        ..Default::default()
    };
    let baseline = prepare(&policy)
        .unwrap()
        .expect("ordinary private storage is admitted");
    assert_eq!(
        baseline.directory,
        workspace.path().canonicalize().unwrap().join(".harn-tmp")
    );
    std::fs::remove_dir_all(workspace.path().join(".harn-tmp")).unwrap();
    std::os::unix::fs::symlink(outside.path(), workspace.path().join(".harn-tmp")).unwrap();
    assert!(
        prepare(&policy).unwrap().is_none(),
        "a redirected scratch root must supply no binding"
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}
