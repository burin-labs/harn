use super::*;
use crate::package::test_support::*;

#[path = "additional.rs"]
mod additional;

#[test]
fn locked_cache_source_holds_entry_lock_until_copy_finishes() {
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
    let commit = entry.commit.as_deref().unwrap();
    let source =
        resolve_locked_package_source_dir(workspace.env(), None, entry, None, true).unwrap();
    let lock_path = git_cache_lock_path_in(workspace.env(), &entry.source, commit).unwrap();
    let contender = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .unwrap();
    assert!(
        matches!(contender.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "a verified cache source must retain its entry lock until its copy is complete"
    );

    let destination = project.path().join("copied");
    copy_dir_recursive(source.as_ref(), &destination).unwrap();
    verify_content_hash(&destination, entry.content_hash.as_deref().unwrap()).unwrap();
    drop(source);

    ensure_git_cache_populated_in(
        workspace.env(),
        &git,
        &entry.source,
        commit,
        entry.content_hash.as_deref(),
        true,
        false,
    )
    .unwrap();
    assert_eq!(clean_package_cache_in(workspace.env(), true).unwrap(), 1);
    verify_content_hash(&destination, entry.content_hash.as_deref().unwrap()).unwrap();
}
