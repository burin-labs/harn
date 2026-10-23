use super::*;
use crate::package::test_support::*;

#[test]
fn installed_source_holds_generation_lease_until_manifest_reads_finish() {
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
    let context = workspace.env().load_manifest_context().unwrap();
    let lock = LockFile::load(&context.lock_path()).unwrap().unwrap();
    let entry = lock.find("acme-lib").unwrap();
    let source =
        materialized_package_dir(&context, "acme-lib", entry.content_hash.as_deref().unwrap())
            .unwrap();
    let directory: &Path = source.as_ref();
    let generation = directory.parent().unwrap().parent().unwrap();
    let contender = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(generation.join(harn_modules::package_snapshot::GENERATION_LEASE_FILE))
        .unwrap();
    assert!(
        matches!(contender.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "installed source must retain the generation lease until its manifest and provenance have been read"
    );
    assert_eq!(
        read_lock_entry_provenance(directory)
            .unwrap()
            .package_version
            .as_deref(),
        Some("0.1.0")
    );
    assert!(read_package_manifest_from_dir(directory).unwrap().is_some());
    drop(source);
    contender.try_lock().unwrap();
    contender.unlock().unwrap();
}
