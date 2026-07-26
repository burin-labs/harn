//! Cache content hashing, verification, and pruning.

use crate::package::test_support::*;
use crate::package::*;

#[test]
fn compute_content_hash_ignores_git_and_hash_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(root.join(".gitignore"), "ignored\n").unwrap();
    fs::write(root.join(CONTENT_HASH_FILE), "stale\n").unwrap();
    fs::write(
        root.join("lib.harn"),
        "pub fn value() -> number { return 1 }\n",
    )
    .unwrap();
    let first = compute_content_hash(root).unwrap();
    fs::write(root.join(".git/HEAD"), "changed\n").unwrap();
    fs::write(root.join(".gitignore"), "changed\n").unwrap();
    fs::write(root.join(CONTENT_HASH_FILE), "changed\n").unwrap();
    let second = compute_content_hash(root).unwrap();
    assert_eq!(first, second);
}

#[test]
fn package_cache_verify_detects_tampering_even_with_stale_marker() {
    let (_repo_tmp, repo, _branch) = create_git_package_repo();
    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let workspace = TestWorkspace::new(root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    fs::write(
        root.join(MANIFEST),
        format!(
            r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = {{ git = "{git}", rev = "v1.0.0" }}
"#
        ),
    )
    .unwrap();

    install_packages_in(workspace.env(), false, None, false).unwrap();
    let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
    let entry = lock.find("acme-lib").unwrap();
    let cache_dir = git_cache_dir_in(
        workspace.env(),
        &entry.source,
        entry.commit.as_deref().unwrap(),
    )
    .unwrap();
    fs::write(
        cache_dir.join("lib.harn"),
        "pub fn value() { return \"pwned\" }\n",
    )
    .unwrap();

    let error = verify_package_cache_in(workspace.env(), false).unwrap_err();
    assert!(error.to_string().contains("content hash mismatch"));
}

#[test]
fn package_cache_clean_all_removes_cached_git_entries() {
    let (_repo_tmp, repo, _branch) = create_git_package_repo();
    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let workspace = TestWorkspace::new(root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    fs::write(
        root.join(MANIFEST),
        format!(
            r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = {{ git = "{git}", rev = "v1.0.0" }}
"#
        ),
    )
    .unwrap();

    install_packages_in(workspace.env(), false, None, false).unwrap();
    assert_eq!(
        discover_package_cache_entries_in(workspace.env())
            .unwrap()
            .len(),
        1
    );

    let removed = clean_package_cache_in(workspace.env(), true).unwrap();
    assert_eq!(removed, 1);
    assert!(discover_package_cache_entries_in(workspace.env())
        .unwrap()
        .is_empty());
}
