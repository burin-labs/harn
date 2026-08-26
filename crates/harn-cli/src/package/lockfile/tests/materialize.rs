//! Realizing a lock file into `packages/`.

use crate::package::test_support::*;
use crate::package::*;

#[test]
fn concurrent_materialization_serializes_package_tree_updates() {
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
    fs::write(
        current_packages_dir(root).join("acme-lib").join("lib.harn"),
        "pub fn value() -> string { return \"stale\" }\n",
    )
    .unwrap();

    let ctx = workspace.env().load_manifest_context().unwrap();
    let lock = LockFile::load(&ctx.lock_path()).unwrap().unwrap();
    let workspace_env = workspace.env().clone();
    let handles = (0..8)
        .map(|_| {
            let workspace_env = workspace_env.clone();
            let ctx = ctx.clone();
            let lock = lock.clone();
            std::thread::spawn(move || {
                materialize_dependencies_from_lock(&workspace_env, &ctx, &lock, None, false)
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let materialized =
        fs::read_to_string(current_packages_dir(root).join("acme-lib").join("lib.harn")).unwrap();
    assert!(materialized.contains("return \"v1\""));
}

#[test]
fn disjoint_demand_publications_preserve_one_cumulative_generation() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let workspace = TestWorkspace::new(root);
    fs::create_dir_all(root.join(".git")).unwrap();
    for alias in ["alpha", "beta"] {
        let dependency = root.join("vendor").join(alias);
        fs::create_dir_all(&dependency).unwrap();
        fs::write(
            dependency.join(MANIFEST),
            format!("[package]\nname = \"{alias}\"\n"),
        )
        .unwrap();
        fs::write(
            dependency.join("lib.harn"),
            format!("pub fn value() -> string {{ return \"{alias}\" }}\n"),
        )
        .unwrap();
    }
    fs::write(
        root.join(MANIFEST),
        r#"
[package]
name = "cumulative-demand-fixture"

[dependencies]
alpha = { path = "./vendor/alpha" }
beta = { path = "./vendor/beta" }
"#,
    )
    .unwrap();
    install_packages_in(workspace.env(), false, None, false).unwrap();
    fs::remove_dir_all(root.join(".harn")).unwrap();
    let entry = root.join("main.harn");
    fs::write(&entry, "pipeline main() {}\n").unwrap();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles = ["alpha", "beta"].map(|alias| {
        let entry = entry.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            ensure_dependency_alias_materialized_for_test(&entry, alias)
        })
    });
    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let snapshot = harn_modules::package_snapshot::PackageSnapshot::acquire(root)
        .unwrap()
        .unwrap();
    let lock = LockFile::load(snapshot.lock_path()).unwrap().unwrap();
    assert_eq!(
        lock.packages
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    for alias in ["alpha", "beta"] {
        assert!(snapshot
            .packages_root()
            .join(alias)
            .join("lib.harn")
            .is_file());
    }
}

#[test]
fn cumulative_demand_reuses_an_installed_remote_package_without_refetching() {
    let (_repo_tmp, repo, _branch) = create_git_package_repo();
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    let workspace = TestWorkspace::new(root);
    fs::create_dir_all(root.join(".git")).unwrap();
    let beta = root.join("vendor/beta");
    fs::create_dir_all(&beta).unwrap();
    fs::write(beta.join(MANIFEST), "[package]\nname = \"beta\"\n").unwrap();
    fs::write(beta.join("lib.harn"), "pub fn value() { 2 }\n").unwrap();
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    fs::write(
        root.join(MANIFEST),
        format!(
            r#"
[package]
name = "cumulative-remote-fixture"

[dependencies]
acme-lib = {{ git = "{git}", tag = "v1.0.0" }}
beta = {{ path = "./vendor/beta" }}
"#,
        ),
    )
    .unwrap();
    install_packages_in(workspace.env(), false, None, false).unwrap();
    fs::remove_dir_all(root.join(".harn")).unwrap();
    let entry = root.join("main.harn");
    fs::write(&entry, "pipeline main() {}\n").unwrap();

    ensure_dependency_alias_materialized_for_test(&entry, "acme-lib").unwrap();
    fs::remove_dir_all(&repo).unwrap();
    fs::remove_dir_all(workspace.cache_dir()).unwrap();
    ensure_dependency_alias_materialized_for_test(&entry, "beta").unwrap();

    let snapshot = harn_modules::package_snapshot::PackageSnapshot::acquire(root)
        .unwrap()
        .unwrap();
    assert!(snapshot.packages_root().join("acme-lib/lib.harn").is_file());
    assert!(snapshot.packages_root().join("beta/lib.harn").is_file());
    assert!(
        !workspace.cache_dir().exists(),
        "cumulative publication must copy the immutable installed source"
    );
}

#[test]
fn materialization_rejects_lock_alias_path_traversal_before_removing_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let dep = tmp.path().join("dep");
    fs::create_dir_all(&dep).unwrap();
    fs::write(dep.join("lib.harn"), "pub fn dep() { 1 }\n").unwrap();
    let victim = tmp.path().join("victim");
    fs::create_dir_all(&victim).unwrap();
    fs::write(victim.join("keep.txt"), "keep").unwrap();

    let manifest: Manifest = toml::from_str("[package]\nname = \"root\"\n").unwrap();
    let ctx = ManifestContext {
        manifest,
        dir: tmp.path().to_path_buf(),
    };
    let workspace = TestWorkspace::new(tmp.path());
    let lock = LockFile {
        packages: vec![LockEntry {
            name: "../victim".to_string(),
            source: path_source_uri(&dep).unwrap(),
            ..LockEntry::default()
        }],
        ..LockFile::default()
    };

    let error =
        materialize_dependencies_from_lock(workspace.env(), &ctx, &lock, None, false).unwrap_err();
    assert!(error.to_string().contains("invalid dependency alias"));
    assert!(
        victim.join("keep.txt").exists(),
        "malicious alias should not remove paths outside the materialization root"
    );
}

/// A project whose dependency source can no longer be reached at all.
///
/// Installs `acme-lib` from a Git source, then removes both the source repo and
/// the package cache, so any attempt to reach the source must fail rather than
/// quietly succeed from a warm cache.
fn project_with_unreachable_source() -> (tempfile::TempDir, TestWorkspace, PathBuf) {
    let (_repo_tmp, repo, _branch) = create_git_package_repo();
    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path().to_path_buf();
    let workspace = TestWorkspace::new(&root);
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

    fs::remove_dir_all(&repo).unwrap();
    fs::remove_dir_all(workspace.cache_dir()).unwrap();

    (project_tmp, workspace, root)
}

/// Every run, nested ones included, is served by the materialized generation.
///
/// A run that reaches its dependency source is indistinguishable from one that
/// reuses what is already materialized as long as the source stays reachable,
/// so the source is deleted first: reuse is the only way to succeed. A run
/// spawned inside a sandbox with no egress, or one whose credentials were
/// scrubbed on the way to the child, is the same situation as this test.
///
/// The paired negative below is what keeps this honest. Without it, this test
/// would still pass if materialization stopped happening for some unrelated
/// reason.
#[test]
fn materialization_reuses_the_generation_without_reaching_the_source() {
    let (_project_tmp, workspace, root) = project_with_unreachable_source();
    let installed = harn_modules::package_snapshot::PackageSnapshot::acquire(&root)
        .unwrap()
        .unwrap();
    let generation = installed.generation().to_string();
    drop(installed);

    ensure_dependencies_materialized_in(workspace.env(), &root).unwrap();

    let snapshot = harn_modules::package_snapshot::PackageSnapshot::acquire(&root)
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.generation(),
        generation,
        "reuse must keep the published generation, not republish an identical one"
    );
    assert!(snapshot
        .packages_root()
        .join("acme-lib")
        .join("lib.harn")
        .is_file());
    // The cache was removed above, so the cache tree reappearing is proof that
    // a fetch was attempted. Asserting on the absence of the fetch itself,
    // rather than on the run's exit status, is what makes this a regression
    // test for the fetch and not merely for the outcome.
    assert!(
        !workspace.cache_dir().exists(),
        "reuse must not populate the package cache, which only a fetch does"
    );
}

#[test]
fn materialization_without_a_generation_reports_the_unreachable_source() {
    let (_project_tmp, workspace, root) = project_with_unreachable_source();
    fs::remove_dir_all(root.join(".harn")).unwrap();

    let error = ensure_dependencies_materialized_in(workspace.env(), &root).unwrap_err();

    let message = error.to_string();
    assert!(
        message.contains("acme-lib") || message.contains("failed to fetch"),
        "a true cache miss must report the unreachable source, got: {message}"
    );
    assert!(
        message.contains("isolated Git environment"),
        "a failed remote call must name the environment it ran in, got: {message}"
    );
}

#[test]
fn read_only_materialization_accepts_v4_git_hashes_without_rewriting_the_lock() {
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
    let cache_dir = git_cache_dir_in(
        workspace.env(),
        &entry.source,
        entry.commit.as_deref().unwrap(),
    )
    .unwrap();
    let v4_hash = compute_archive_content_hash(&cache_dir).unwrap();
    let v4_lock = fs::read_to_string(&lock_path)
        .unwrap()
        .replace("version = 5", "version = 4")
        .replace(canonical_hash, &v4_hash);
    fs::write(&lock_path, &v4_lock).unwrap();

    ensure_dependencies_materialized_in(workspace.env(), root).unwrap();

    assert_eq!(fs::read_to_string(&lock_path).unwrap(), v4_lock);
    assert!(current_packages_dir(root)
        .join("acme-lib")
        .join("lib.harn")
        .is_file());
    let snapshot = harn_modules::package_snapshot::PackageSnapshot::acquire(root)
        .unwrap()
        .unwrap();
    let runtime_lock = fs::read_to_string(snapshot.lock_path()).unwrap();
    assert!(runtime_lock.contains(canonical_hash));
    assert!(!runtime_lock.contains(&v4_hash));
}
