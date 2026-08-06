//! Install, update, and frozen/offline lock behavior end to end.

use super::fixtures::write_tar_gz_package_archive;

use crate::package::test_support::*;
use crate::package::*;

#[test]
fn install_resolves_git_tag_dependency_and_records_tag() {
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
acme-lib = {{ git = "{git}", tag = "v1.0.0" }}
"#
        ),
    )
    .unwrap();

    let installed = install_packages_in(workspace.env(), false, None, false).unwrap();

    assert_eq!(installed, 1);
    let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
    let entry = lock.find("acme-lib").unwrap();
    assert_eq!(entry.tag.as_deref(), Some("v1.0.0"));
    assert_eq!(entry.rev_request.as_deref(), Some("v1.0.0"));
    assert!(entry.commit.as_deref().is_some_and(is_full_git_sha));
    assert!(entry.content_hash.as_deref().is_some());
    assert!(current_packages_dir(root)
        .join("acme-lib")
        .join("lib.harn")
        .is_file());
}

#[test]
fn install_migrates_v4_git_hashes_to_canonical_v2() {
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
acme-lib = {{ git = "{git}", tag = "v1.0.0" }}
"#
        ),
    )
    .unwrap();

    install_packages_in(workspace.env(), false, None, false).unwrap();
    let lock_path = root.join(LOCK_FILE);
    let current = LockFile::load(&lock_path).unwrap().unwrap();
    let entry = current.find("acme-lib").unwrap();
    let canonical_hash = entry.content_hash.as_deref().unwrap();
    assert!(is_canonical_content_hash(canonical_hash));
    let cache_dir = git_cache_dir_in(
        workspace.env(),
        &entry.source,
        entry.commit.as_deref().unwrap(),
    )
    .unwrap();
    let v4_hash = compute_archive_content_hash(&cache_dir).unwrap();
    assert!(!is_canonical_content_hash(&v4_hash));
    let v4_lock = fs::read_to_string(&lock_path)
        .unwrap()
        .replace("version = 5", "version = 4")
        .replace(canonical_hash, &v4_hash);
    fs::write(&lock_path, &v4_lock).unwrap();

    let locked_error = install_packages_in(workspace.env(), true, None, false).unwrap_err();
    assert!(
        locked_error
            .to_string()
            .contains("run `harn install` and commit the migrated lockfile"),
        "{locked_error}"
    );
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), v4_lock);

    install_packages_in(workspace.env(), false, None, false).unwrap();
    let migrated = LockFile::load(&lock_path).unwrap().unwrap();
    let migrated_entry = migrated.find("acme-lib").unwrap();
    assert_eq!(migrated.version, LOCK_FILE_VERSION);
    assert_eq!(migrated_entry.commit, entry.commit);
    assert_eq!(migrated_entry.content_hash.as_deref(), Some(canonical_hash));
}

#[test]
fn install_resolves_registry_version_range_to_highest_matching_tag() {
    let (_repo_tmp, repo, _branch) = create_git_package_repo();
    fs::write(
        repo.join("lib.harn"),
        "pub fn value() -> string { return \"v0.1.1\" }\n",
    )
    .unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "v0.1.1"]);
    run_git(&repo, &["tag", "v0.1.1"]);
    fs::write(
        repo.join("lib.harn"),
        "pub fn value() -> string { return \"v0.2.0\" }\n",
    )
    .unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "v0.2.0"]);
    run_git(&repo, &["tag", "v0.2.0"]);

    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let registry_path = root.join("index.toml");
    let workspace =
        TestWorkspace::new(root).with_registry_source(registry_path.display().to_string());
    fs::create_dir_all(root.join(".git")).unwrap();
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    let rev_100 = run_git(&repo, &["rev-parse", "v1.0.0"]);
    let rev_011 = run_git(&repo, &["rev-parse", "v0.1.1"]);
    let rev_020 = run_git(&repo, &["rev-parse", "v0.2.0"]);
    fs::write(
        &registry_path,
        format!(
            r#"
version = 2

[[package]]
name = "acme-lib"
repository = "{git}"
provenance = "{git}"

[[package.version]]
version = "0.1.0"
git = "{git}"
tag = "v1.0.0"
rev = "{rev_100}"
provenance = "{git}"

[[package.version]]
version = "0.1.1"
git = "{git}"
tag = "v0.1.1"
rev = "{rev_011}"
provenance = "{git}"

[[package.version]]
version = "0.2.0"
git = "{git}"
tag = "v0.2.0"
rev = "{rev_020}"
provenance = "{git}"
"#
        ),
    )
    .unwrap();
    fs::write(
        root.join(MANIFEST),
        r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
acme-lib = { version = ">=0.1,<0.2" }
"#,
    )
    .unwrap();

    let installed = install_packages_in(workspace.env(), false, None, false).unwrap();

    assert_eq!(installed, 1);
    let lock_path = root.join(LOCK_FILE);
    let lock = LockFile::load(&lock_path).unwrap().unwrap();
    let entry = lock.find("acme-lib").unwrap();
    assert_eq!(entry.tag.as_deref(), Some("v0.1.1"));
    assert_eq!(entry.rev_request.as_deref(), Some("v0.1.1"));
    assert_eq!(
        entry
            .registry
            .as_ref()
            .map(|registry| registry.version.as_str()),
        Some("0.1.1")
    );
    let source =
        fs::read_to_string(current_packages_dir(root).join("acme-lib").join("lib.harn")).unwrap();
    assert!(source.contains("v0.1.1"), "{source}");

    let original_lock = fs::read_to_string(&lock_path).unwrap();
    fs::remove_dir_all(current_packages_dir(root)).unwrap();
    fs::remove_dir_all(&repo).unwrap();
    fs::remove_file(&registry_path).unwrap();

    let reinstalled = install_packages_in(workspace.env(), true, None, true).unwrap();
    assert_eq!(reinstalled, 1);
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), original_lock);
    assert!(current_packages_dir(root)
        .join("acme-lib")
        .join("lib.harn")
        .is_file());
}

#[test]
fn registry_archive_dependency_materializes_and_reinstalls_offline() {
    let package_tmp = tempfile::tempdir().unwrap();
    let package_root = package_tmp.path().join("acme-rules");
    fs::create_dir_all(package_root.join("rules")).unwrap();
    fs::write(
        package_root.join(MANIFEST),
        r#"
[package]
name = "acme-rules"
version = "1.0.0"

[rules]
ruleDirs = ["rules"]
"#,
    )
    .unwrap();
    fs::write(
        package_root.join("rules/no_todo.harn"),
        "pub fn rule() -> string { return \"no todo\" }\n",
    )
    .unwrap();
    let checksum = compute_archive_content_hash(&package_root).unwrap();

    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let registry_path = root.join("index.toml");
    let archive_path = root.join("acme-rules-1.0.0.tar.gz");
    write_tar_gz_package_archive(&package_root, &archive_path);
    let archive = normalize_archive_url(archive_path.to_string_lossy().as_ref()).unwrap();
    let workspace =
        TestWorkspace::new(root).with_registry_source(registry_path.display().to_string());
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(
        &registry_path,
        format!(
            r#"
version = 2

[[package]]
name = "@acme/rules"
description = "Rule pack"
repository = "https://github.com/acme/rules"
provenance = "https://github.com/acme/rules"

[package.rule_pack]
rule_count = 1
languages = ["harn"]
safety_summary = ["advisory:1"]

[[package.version]]
version = "1.0.0"
archive = "{archive}"
package = "acme-rules"
checksum = "{checksum}"
provenance = "https://github.com/acme/rules/releases/tag/v1.0.0"
"#
        ),
    )
    .unwrap();
    fs::write(
        root.join(MANIFEST),
        r#"
[package]
name = "workspace"
version = "0.1.0"
"#,
    )
    .unwrap();

    let (alias, installed) = add_package_to(
        workspace.env(),
        "@acme/rules@1.0.0",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    assert_eq!(alias, "acme-rules");
    assert_eq!(installed, 1);
    let manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
    assert!(manifest.contains("archive = "));
    assert!(manifest.contains(&format!("checksum = \"{checksum}\"")));
    assert!(manifest.contains("registry_name = \"@acme/rules\""));
    let lock_path = root.join(LOCK_FILE);
    let lock = LockFile::load(&lock_path).unwrap().unwrap();
    let entry = lock.find("acme-rules").unwrap();
    assert!(entry.source.starts_with("archive+file://"));
    assert_eq!(entry.content_hash.as_deref(), Some(checksum.as_str()));
    assert!(entry.commit.is_none());
    assert_eq!(
        entry
            .registry
            .as_ref()
            .map(|registry| registry.name.as_str()),
        Some("@acme/rules")
    );
    assert!(current_packages_dir(root)
        .join("acme-rules")
        .join("rules/no_todo.harn")
        .is_file());

    let original_lock = fs::read_to_string(&lock_path).unwrap();
    fs::remove_dir_all(current_packages_dir(root)).unwrap();
    fs::remove_file(&archive_path).unwrap();
    let reinstalled = install_packages_in(workspace.env(), true, None, true).unwrap();
    assert_eq!(reinstalled, 1);
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), original_lock);
    assert!(current_packages_dir(root)
        .join("acme-rules")
        .join("rules/no_todo.harn")
        .is_file());
}

#[test]
fn update_branch_dependency_refreshes_locked_commit() {
    let (_repo_tmp, repo, branch) = create_git_package_repo();
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
acme-lib = {{ git = "{git}", branch = "{branch}" }}
"#
        ),
    )
    .unwrap();

    let installed = install_packages_in(workspace.env(), false, None, false).unwrap();
    assert_eq!(installed, 1);
    let first_lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
    let first_commit = first_lock
        .find("acme-lib")
        .and_then(|entry| entry.commit.clone())
        .unwrap();

    fs::write(
        repo.join("lib.harn"),
        "pub fn value() -> string { return \"v2\" }\n",
    )
    .unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "update"]);

    update_packages_in(workspace.env(), Some("acme-lib"), false).unwrap();
    let second_lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
    let second_commit = second_lock
        .find("acme-lib")
        .and_then(|entry| entry.commit.clone())
        .unwrap();
    assert_ne!(first_commit, second_commit);
}

#[test]
fn frozen_install_errors_when_lockfile_is_missing() {
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

    let error = install_packages_in(workspace.env(), true, None, false).unwrap_err();
    assert!(error.to_string().contains(LOCK_FILE));
}

#[test]
fn frozen_install_tolerates_provenance_stamp_drift() {
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

    let installed = install_packages_in(workspace.env(), false, None, false).unwrap();
    assert_eq!(installed, 1);

    // Simulate a lock written by an older Harn release: identical
    // resolution, stale provenance stamps. A release bump must not
    // break `harn install --locked`.
    let lock_path = root.join(LOCK_FILE);
    let stale = fs::read_to_string(&lock_path)
        .unwrap()
        .replace(
            &format!("generator_version = \"{}\"", current_generator_version()),
            "generator_version = \"0.0.1\"",
        )
        .replace(
            &format!(
                "protocol_artifact_version = \"{}\"",
                current_protocol_artifact_version()
            ),
            "protocol_artifact_version = \"0.0.1\"",
        );
    assert!(
        stale.contains("generator_version = \"0.0.1\""),
        "test should have rewritten the provenance stamps: {stale}"
    );
    fs::write(&lock_path, stale).unwrap();

    let installed = install_packages_in(workspace.env(), true, None, false).unwrap();
    assert_eq!(installed, 1);

    // Frozen install must not rewrite the lock (the stale stamps stay
    // until a non-frozen install refreshes provenance).
    let after = fs::read_to_string(&lock_path).unwrap();
    assert!(after.contains("generator_version = \"0.0.1\""));
}

#[test]
fn frozen_install_errors_when_manifest_dropped_all_dependencies() {
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

    // Manifest drops its dependencies but the stale lock still pins
    // them: frozen mode must flag the pending lock change instead of
    // silently succeeding.
    fs::write(
        root.join(MANIFEST),
        r#"
[package]
name = "workspace"
version = "0.1.0"
"#,
    )
    .unwrap();

    let error = install_packages_in(workspace.env(), true, None, false).unwrap_err();
    assert!(error.to_string().contains("would need to change"));

    // An empty lock (no packages) is fine in frozen mode.
    LockFile::default().save(&root.join(LOCK_FILE)).unwrap();
    let installed = install_packages_in(workspace.env(), true, None, false).unwrap();
    assert_eq!(installed, 0);
}

#[test]
fn offline_locked_install_materializes_from_cache_without_source_repo() {
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

    let installed = install_packages_in(workspace.env(), false, None, false).unwrap();
    assert_eq!(installed, 1);
    fs::remove_dir_all(current_packages_dir(root)).unwrap();
    fs::remove_dir_all(&repo).unwrap();

    let installed = install_packages_in(workspace.env(), true, None, true).unwrap();
    assert_eq!(installed, 1);
    assert!(current_packages_dir(root)
        .join("acme-lib")
        .join("lib.harn")
        .is_file());
}

#[test]
fn offline_locked_install_fails_when_cache_is_missing() {
    let (_repo_tmp, repo, _branch) = create_git_package_repo();
    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let workspace = TestWorkspace::new(root);
    let cache_dir = workspace.cache_dir();
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
    fs::remove_dir_all(cache_dir.join("git")).unwrap();
    // The generation holds the same content, and resolution will now read the
    // dependency's manifest from there rather than refetching. Remove it too,
    // so this stays a test about content being available nowhere.
    fs::remove_dir_all(current_packages_dir(root)).unwrap();
    let error = install_packages_in(workspace.env(), true, None, true).unwrap_err();
    assert!(error.to_string().contains("offline mode"));
}

/// Resolution reads each dependency's manifest to walk transitive dependencies.
/// It used to take that read from the package cache, so populating the cache --
/// a network fetch when cold -- was required even by a fully pinned lock. The
/// generation already holds that content at the hash the lock pins, so it can
/// answer the same question without the source being reachable at all.
#[test]
fn offline_locked_install_resolves_from_the_generation_when_the_cache_is_gone() {
    let (_repo_tmp, repo, _branch) = create_git_package_repo();
    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let workspace = TestWorkspace::new(root);
    let cache_dir = workspace.cache_dir();
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
    // Neither the cache nor the origin survives; only the materialized
    // generation does.
    fs::remove_dir_all(cache_dir.join("git")).unwrap();
    fs::remove_dir_all(&repo).unwrap();

    let installed = install_packages_in(workspace.env(), true, None, true).unwrap();

    assert_eq!(installed, 1);
    assert!(current_packages_dir(root)
        .join("acme-lib")
        .join("lib.harn")
        .is_file());
}

#[test]
fn install_resolves_transitive_git_dependencies_from_clean_cache() {
    let (_sdk_tmp, sdk_repo, _branch) = create_git_package_repo_with(
        "notion-sdk-harn",
        "",
        "pub fn sdk_value() -> string { return \"sdk\" }\n",
    );
    let sdk_git = normalize_git_url(sdk_repo.to_string_lossy().as_ref()).unwrap();
    let connector_tail = format!(
        r#"

[dependencies]
notion-sdk-harn = {{ git = "{sdk_git}", rev = "v1.0.0" }}
"#
    );
    let (_connector_tmp, connector_repo, _branch) = create_git_package_repo_with(
        "notion-connector-harn",
        &connector_tail,
        r#"
import "notion-sdk-harn"

pub fn connector_value() -> string {
  return "connector"
}
"#,
    );

    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let workspace = TestWorkspace::new(root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let connector_git = normalize_git_url(connector_repo.to_string_lossy().as_ref()).unwrap();
    fs::write(
        root.join(MANIFEST),
        format!(
            r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
notion-connector-harn = {{ git = "{connector_git}", rev = "v1.0.0" }}
"#
        ),
    )
    .unwrap();

    let installed = install_packages_in(workspace.env(), false, None, false).unwrap();
    assert_eq!(installed, 2);

    let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
    assert!(lock.find("notion-connector-harn").is_some());
    assert!(lock.find("notion-sdk-harn").is_some());
    assert!(current_packages_dir(root)
        .join("notion-connector-harn")
        .join("lib.harn")
        .is_file());
    assert!(current_packages_dir(root)
        .join("notion-sdk-harn")
        .join("lib.harn")
        .is_file());

    let mut vm = test_vm();
    let exports = futures::executor::block_on(
        vm.load_module_exports(
            &current_packages_dir(root)
                .join("notion-connector-harn")
                .join("lib.harn"),
        ),
    )
    .expect("transitive import should load from the workspace package root");
    assert!(exports.contains_key("connector_value"));
}

#[test]
fn git_packages_reject_transitive_path_dependencies() {
    let connector_tail = r#"

[dependencies]
local-helper = { path = "../helper" }
"#;
    let (_connector_tmp, connector_repo, _branch) = create_git_package_repo_with(
        "notion-connector-harn",
        connector_tail,
        "pub fn connector_value() -> string { return \"connector\" }\n",
    );

    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let workspace = TestWorkspace::new(root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let connector_git = normalize_git_url(connector_repo.to_string_lossy().as_ref()).unwrap();
    fs::write(
        root.join(MANIFEST),
        format!(
            r#"
[package]
name = "workspace"
version = "0.1.0"

[dependencies]
notion-connector-harn = {{ git = "{connector_git}", rev = "v1.0.0" }}
"#
        ),
    )
    .unwrap();

    let error = install_packages_in(workspace.env(), false, None, false).unwrap_err();
    assert!(error
        .to_string()
        .contains("path dependencies are not supported inside remote-installed packages"));
}
