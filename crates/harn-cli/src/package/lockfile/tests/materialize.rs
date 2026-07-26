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
