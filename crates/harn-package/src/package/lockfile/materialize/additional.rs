use super::*;

#[test]
fn archive_source_holds_entry_and_cleanup_locks_until_copy_finishes() {
    let project = tempfile::tempdir().unwrap();
    let workspace = TestWorkspace::new(project.path());
    let package = project.path().join("source");
    fs::create_dir(&package).unwrap();
    fs::write(
        package.join(MANIFEST),
        "[package]\nname = \"acme-lib\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    fs::write(
        package.join("lib.harn"),
        "pub fn value() -> string { return \"archive\" }\n",
    )
    .unwrap();
    let expected_hash = compute_archive_content_hash(&package).unwrap();
    let archive_path = project.path().join("source.tar.gz");
    let encoder = flate2::write::GzEncoder::new(
        File::create(&archive_path).unwrap(),
        flate2::Compression::default(),
    );
    let mut archive = tar::Builder::new(encoder);
    for name in [MANIFEST, "lib.harn"] {
        archive
            .append_path_with_name(package.join(name), name)
            .unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap();
    let url = normalize_archive_url(archive_path.to_str().unwrap()).unwrap();
    fs::write(
        project.path().join(MANIFEST),
        format!(
            "[dependencies]\nacme-lib = {{ archive = \"{url}\", checksum = \"{expected_hash}\" }}\n"
        ),
    )
    .unwrap();
    install_packages_in(workspace.env(), false, None, false).unwrap();
    let lock = LockFile::load(&project.path().join(LOCK_FILE))
        .unwrap()
        .unwrap();
    let entry = lock.find("acme-lib").unwrap();
    let source =
        resolve_locked_package_source_dir(workspace.env(), None, entry, None, true).unwrap();
    assert_lock_is_held(
        &archive_cache_lock_path_in(workspace.env(), &entry.source, &expected_hash).unwrap(),
    );
    assert_lock_is_held(&workspace.cache_dir().join("cache.lock"));
    let copied = project.path().join("copied");
    copy_dir_recursive(source.as_ref(), &copied).unwrap();
    verify_content_hash(&copied, &expected_hash).unwrap();
    drop(source);
    ensure_archive_cache_populated_in(
        workspace.env(),
        &url,
        &entry.source,
        &expected_hash,
        true,
        false,
    )
    .unwrap();
    assert_eq!(clean_package_cache_in(workspace.env(), true).unwrap(), 1);
    verify_content_hash(&copied, &expected_hash).unwrap();
}

fn assert_lock_is_held(path: &Path) {
    let contender = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    assert!(
        matches!(contender.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "cache lock must remain held: {}",
        path.display()
    );
}

#[test]
fn cleanup_preserves_the_lifetime_lock_for_all_and_unused_entries() {
    let (_repository, repo, _branch) = create_git_package_repo();
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    for all in [true, false] {
        let project = tempfile::tempdir().unwrap();
        let workspace = TestWorkspace::new(project.path());
        fs::write(
            project.path().join(MANIFEST),
            format!("[dependencies]\nacme-lib = {{ git = \"{git}\", tag = \"v1.0.0\" }}\n"),
        )
        .unwrap();
        install_packages_in(workspace.env(), false, None, false).unwrap();
        let old_lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(workspace.cache_dir().join("cache.lock"))
            .unwrap();
        if !all {
            fs::write(project.path().join(MANIFEST), "[dependencies]\n").unwrap();
            install_packages_in(workspace.env(), false, None, false).unwrap();
        }
        assert_eq!(clean_package_cache_in(workspace.env(), all).unwrap(), 1);
        let reader = PackageCache::inspect(workspace.env()).unwrap().unwrap();
        assert!(
            matches!(old_lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
            "cleanup must retain the root lock's file identity"
        );
        drop(reader);
        old_lock.try_lock().unwrap();
        old_lock.unlock().unwrap();
    }
}

#[test]
fn cache_inspection_does_not_create_an_absent_cache() {
    let project = tempfile::tempdir().unwrap();
    let workspace = TestWorkspace::new(project.path());
    assert!(discover_package_cache_entries_in(workspace.env())
        .unwrap()
        .is_empty());
    assert!(!workspace.cache_dir().exists());
}

#[test]
fn cache_inspection_rejects_entries_without_a_lifetime_lock() {
    let project = tempfile::tempdir().unwrap();
    let workspace = TestWorkspace::new(project.path());
    fs::create_dir_all(workspace.cache_dir().join("git/source/commit")).unwrap();
    let error = discover_package_cache_entries_in(workspace.env()).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("package cache lifetime lock is missing"),
        "{error}"
    );
    assert!(!workspace.cache_dir().join("cache.lock").exists());
}

#[test]
fn new_cache_layout_rejects_legacy_entries_but_reuses_installed_generations() {
    let (_repository, repo, _branch) = create_git_package_repo();
    let project = tempfile::tempdir().unwrap();
    let workspace = TestWorkspace::new(project.path());
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    fs::write(
        project.path().join(MANIFEST),
        format!("[dependencies]\nacme-lib = {{ git = \"{git}\", tag = \"v1.0.0\" }}\n"),
    )
    .unwrap();
    install_packages_in(workspace.env(), false, None, false).unwrap();
    let lock = LockFile::load(&project.path().join(LOCK_FILE))
        .unwrap()
        .unwrap();
    let entry = lock.find("acme-lib").unwrap();
    let cache_root = workspace.cache_dir();
    assert_eq!(cache_root.file_name().unwrap(), "v2");
    let legacy_git = cache_root.parent().unwrap().join("git");
    fs::rename(cache_root.join("git"), &legacy_git).unwrap();
    let installed =
        InstalledPackageSources::acquire(&workspace.env().load_manifest_context().unwrap())
            .unwrap()
            .unwrap();
    let reused =
        resolve_locked_package_source_dir(workspace.env(), Some(&installed), entry, None, true)
            .unwrap();
    verify_content_hash(reused.as_ref(), entry.content_hash.as_deref().unwrap()).unwrap();
    let error = match resolve_locked_package_source_dir(workspace.env(), None, entry, None, true) {
        Ok(_) => panic!("offline install must not fall back to the legacy cache"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("cannot fetch in offline mode"),
        "{error}"
    );
    assert!(legacy_git.is_dir());
}

#[test]
fn frozen_install_that_reuses_a_generation_still_fills_the_cache_for_an_offline_install() {
    let (_repository, repo, _branch) = create_git_package_repo();
    let project = tempfile::tempdir().unwrap();
    let workspace = TestWorkspace::new(project.path());
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    let manifest = format!("[dependencies]\nacme-lib = {{ git = \"{git}\", tag = \"v1.0.0\" }}\n");
    fs::write(project.path().join(MANIFEST), &manifest).unwrap();
    install_packages_in(workspace.env(), false, None, false).unwrap();
    // A cold cache beside a current generation is what a rebuilt cache
    // directory or a later checkout of the same project looks like.
    fs::remove_dir_all(workspace.cache_dir()).unwrap();
    install_packages_in(workspace.env(), true, None, false).unwrap();

    let downstream = tempfile::tempdir().unwrap();
    fs::write(downstream.path().join(MANIFEST), &manifest).unwrap();
    fs::copy(
        project.path().join(LOCK_FILE),
        downstream.path().join(LOCK_FILE),
    )
    .unwrap();
    let offline = PackageWorkspace::for_test(downstream.path(), project.path().join(".cache"));
    install_packages_in(&offline, true, None, true).unwrap();
}

#[test]
fn distinct_aliases_can_reuse_the_same_cached_source() {
    let (_repository, repo, _branch) = create_git_package_repo();
    let project = tempfile::tempdir().unwrap();
    let workspace = TestWorkspace::new(project.path());
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    fs::write(project.path().join(MANIFEST), format!("[dependencies]\nfirst = {{ git = \"{git}\", tag = \"v1.0.0\" }}\nsecond = {{ git = \"{git}\", tag = \"v1.0.0\" }}\n")).unwrap();
    assert_eq!(
        install_packages_in(workspace.env(), false, None, false).unwrap(),
        2
    );
    let lock = LockFile::load(&project.path().join(LOCK_FILE))
        .unwrap()
        .unwrap();
    assert_eq!(
        lock.find("first").unwrap().content_hash,
        lock.find("second").unwrap().content_hash
    );
    assert_eq!(verify_package_cache_in(workspace.env(), true).unwrap(), 4);
}

#[test]
fn integrity_reads_do_not_repair_cache_or_installed_hash_markers() {
    let (_repository, repo, _branch) = create_git_package_repo();
    let project = tempfile::tempdir().unwrap();
    let workspace = TestWorkspace::new(project.path());
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    fs::write(
        project.path().join(MANIFEST),
        format!("[dependencies]\nacme-lib = {{ git = \"{git}\", tag = \"v1.0.0\" }}\n"),
    )
    .unwrap();
    install_packages_in(workspace.env(), false, None, false).unwrap();
    let lock = LockFile::load(&project.path().join(LOCK_FILE))
        .unwrap()
        .unwrap();
    let entry = lock.find("acme-lib").unwrap();
    let cache_marker = git_cache_dir_in(
        workspace.env(),
        &entry.source,
        entry.commit.as_deref().unwrap(),
    )
    .unwrap()
    .join(CONTENT_HASH_FILE);
    let installed_marker = current_packages_dir(project.path())
        .join("acme-lib")
        .join(CONTENT_HASH_FILE);
    fs::write(&cache_marker, b"stale\n").unwrap();
    fs::write(&installed_marker, b"stale\n").unwrap();
    let report = crate::package::maturity::audit_packages_in(workspace.env(), None, false).unwrap();
    assert!(!report
        .findings
        .iter()
        .any(|finding| matches!(finding.code, AuditCode::ContentHashMismatch)));
    let installed =
        InstalledPackageSources::acquire(&workspace.env().load_manifest_context().unwrap())
            .unwrap()
            .unwrap();
    assert!(installed.source_for(entry).unwrap().is_some());
    assert_eq!(
        fs::read(&cache_marker).unwrap(),
        b"stale\n",
        "audit must not write shared cache bytes"
    );
    assert_eq!(
        fs::read(&installed_marker).unwrap(),
        b"stale\n",
        "generation reads must keep installed bytes immutable"
    );
}

#[test]
fn demand_resolution_rejects_remote_path_dependencies_before_enqueuing() {
    let (_repository, repo, _branch) = create_git_package_repo();
    let project = tempfile::tempdir().unwrap();
    let workspace = TestWorkspace::new(project.path());
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    fs::write(
        project.path().join(MANIFEST),
        format!("[dependencies]\nacme-lib = {{ git = \"{git}\", tag = \"v1.0.0\" }}\n"),
    )
    .unwrap();
    install_packages_in(workspace.env(), false, None, false).unwrap();
    let mut lock = LockFile::load(&project.path().join(LOCK_FILE))
        .unwrap()
        .unwrap();
    let (_remote_repository, remote, _) = create_git_package_repo_with(
        "acme-lib",
        "\n[dependencies]\nlocal-helper = { path = \"../helper\" }\n",
        "pub fn value() -> string { return \"remote\" }\n",
    );
    let remote_git = normalize_git_url(remote.to_str().unwrap()).unwrap();
    fs::write(
        project.path().join(MANIFEST),
        format!("[dependencies]\nacme-lib = {{ git = \"{remote_git}\", tag = \"v1.0.0\" }}\n"),
    )
    .unwrap();
    let mut entry = lock.find("acme-lib").unwrap().clone();
    entry.source = format!("git+{remote_git}");
    entry.commit = Some(resolve_git_commit(&remote_git, None, Some("v1.0.0"), None).unwrap());
    entry.content_hash = Some(compute_content_hash(&remote).unwrap());
    lock.replace(entry);
    let context = workspace.env().load_manifest_context().unwrap();
    let dependency = context
        .manifest
        .dependencies
        .get("acme-lib")
        .unwrap()
        .clone();
    let pending = vec![ReachableDependency {
        alias: "acme-lib".to_string(),
        dependency,
        manifest_dir: context.dir.clone(),
    }];
    let error = dependency_closure_lock(workspace.env(), &context, &lock, pending).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("path dependencies are not supported inside remote-installed packages"),
        "{error}"
    );
}
