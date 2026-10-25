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

#[test]
fn reaping_removes_only_dangling_alias_symlinks() {
    let base = tempfile::tempdir().unwrap();
    let live_target = tempfile::tempdir().unwrap();
    let gone_target = tempfile::tempdir().unwrap();
    let live = prepare_in(base.path(), live_target.path()).unwrap();
    let gone = prepare_in(base.path(), gone_target.path()).unwrap();
    let gone_path = gone_target.path().to_path_buf();
    drop(gone_target);
    assert!(!gone_path.exists());

    // Dangling symlinks that do not match the alias pattern, a matching
    // directory, and a matching regular file must all survive.
    let dangling_other = base.path().join("harn-parent");
    std::os::unix::fs::symlink(&gone_path, &dangling_other).unwrap();
    let wrong_case = base.path().join("harn-ABCDEF0123456789");
    std::os::unix::fs::symlink(&gone_path, &wrong_case).unwrap();
    let directory = base.path().join("harn-0123456789abcdef");
    std::fs::create_dir(&directory).unwrap();
    let file = base.path().join("harn-fedcba9876543210");
    std::fs::write(&file, "host-owned").unwrap();

    reap_dangling_aliases(base.path());

    assert!(
        !base.path().join(&gone).is_symlink(),
        "dangling alias remains"
    );
    assert_eq!(
        std::fs::read_link(base.path().join(&live)).unwrap(),
        live_target.path()
    );
    assert!(dangling_other.is_symlink());
    assert!(wrong_case.is_symlink());
    assert!(directory.is_dir());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "host-owned");
}

#[test]
fn reaping_rechecks_under_the_lock_and_spares_an_alias_that_went_live() {
    let base = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let suffix = prepare_in(base.path(), workspace.path()).unwrap();
    // A scan saw this alias dangling; the workspace came back before the
    // unlink. The re-check must leave it, and a truly dangling one must go.
    reap_if_dangling(base.path(), OsStr::new(&suffix));
    assert_eq!(
        std::fs::read_link(base.path().join(&suffix)).unwrap(),
        workspace.path()
    );
    let gone = tempfile::tempdir().unwrap();
    let dangling = prepare_in(base.path(), gone.path()).unwrap();
    drop(gone);
    reap_if_dangling(base.path(), OsStr::new(&dangling));
    assert!(!base.path().join(&dangling).is_symlink());
}

#[test]
fn a_preparing_spawn_excludes_reapers_until_it_releases_the_alias_lock() {
    use std::os::unix::io::AsRawFd;
    let base = tempfile::tempdir().unwrap();
    let exclusive_now = |base: &Path| {
        let file = std::fs::File::options()
            .create(true)
            .append(true)
            .open(base.join(ALIAS_LOCK))
            .unwrap();
        // SAFETY: the descriptor is open for the life of `file`.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
    };
    let preparing = AliasLock::acquire(base.path(), false).unwrap();
    assert!(
        !exclusive_now(base.path()),
        "a reaper could lock while a spawn was preparing"
    );
    drop(preparing);
    assert!(exclusive_now(base.path()));
}
