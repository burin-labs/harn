//! `harn add` spec normalization and the manifest/lock writes it makes.

use super::super::materialize::dependency_manifest_item;

use crate::package::test_support::*;
use crate::package::*;

#[test]
fn add_and_remove_git_dependency_round_trip() {
    let (_repo_tmp, repo, _branch) = create_git_package_repo();
    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let workspace = TestWorkspace::new(root);
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(
        root.join(MANIFEST),
        r#"
[package]
name = "workspace"
version = "0.1.0"
"#,
    )
    .unwrap();

    let spec = format!("{}@v1.0.0", repo.display());
    add_package_to(
        workspace.env(),
        &spec,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let alias = "acme-lib";
    let manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
    assert!(manifest.contains("acme-lib"));
    assert!(manifest.contains("rev = \"v1.0.0\""));

    let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
    let entry = lock.find(alias).unwrap();
    assert_eq!(lock.version, LOCK_FILE_VERSION);
    assert!(entry.source.starts_with("git+file://"));
    assert!(entry.commit.as_deref().is_some_and(is_full_git_sha));
    assert!(entry
        .content_hash
        .as_deref()
        .is_some_and(|hash| hash.starts_with("sha256:")));
    assert!(current_packages_dir(root)
        .join(alias)
        .join("lib.harn")
        .is_file());

    remove_package_in(workspace.env(), alias).unwrap();
    let updated_manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
    assert!(!updated_manifest.contains("acme-lib ="));
    let updated_lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
    assert!(updated_lock.find(alias).is_none());
    assert!(!current_packages_dir(root).join(alias).exists());
}

#[test]
fn add_positional_local_path_dependency_uses_manifest_name_and_live_link() {
    let dependency_tmp = tempfile::tempdir().unwrap();
    let dependency_root = dependency_tmp.path().join("harn-openapi");
    fs::create_dir_all(&dependency_root).unwrap();
    fs::write(
        dependency_root.join(MANIFEST),
        r#"
[package]
name = "openapi"
version = "0.1.0"
"#,
    )
    .unwrap();
    fs::write(
        dependency_root.join("lib.harn"),
        "pub fn version() -> string { return \"v1\" }\n",
    )
    .unwrap();

    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let workspace = TestWorkspace::new(root);
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(
        root.join(MANIFEST),
        r#"
[package]
name = "workspace"
version = "0.1.0"
"#,
    )
    .unwrap();

    add_package_to(
        workspace.env(),
        dependency_root.to_string_lossy().as_ref(),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();

    let manifest = fs::read_to_string(root.join(MANIFEST)).unwrap();
    assert!(
        manifest.contains("openapi = { path = "),
        "manifest should use package.name as alias: {manifest}"
    );
    let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
    let entry = lock.find("openapi").expect("openapi lock entry");
    assert!(entry.source.starts_with("path+file://"));
    let materialized = current_packages_dir(root).join("openapi");
    assert!(materialized.join("lib.harn").is_file());

    #[cfg(unix)]
    assert!(
        fs::symlink_metadata(&materialized)
            .unwrap()
            .file_type()
            .is_symlink(),
        "path dependencies should be live-linked on Unix"
    );

    #[cfg(windows)]
    let materialized_is_link = fs::symlink_metadata(&materialized)
        .unwrap()
        .file_type()
        .is_symlink();

    fs::write(
        dependency_root.join("lib.harn"),
        "pub fn version() -> string { return \"v2\" }\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        let live_source = fs::read_to_string(materialized.join("lib.harn")).unwrap();
        assert!(
            live_source.contains("v2"),
            "materialized path dependency should reflect sibling repo edits"
        );
    }
    #[cfg(windows)]
    {
        let materialized_source = fs::read_to_string(materialized.join("lib.harn")).unwrap();
        if materialized_is_link {
            assert!(
                materialized_source.contains("v2"),
                "Windows path dependency symlink should reflect sibling repo edits"
            );
        } else {
            assert!(
                materialized_source.contains("v1"),
                "Windows path dependency copy fallback should keep the copied contents"
            );
        }
    }

    remove_package_in(workspace.env(), "openapi").unwrap();
    assert!(!materialized.exists());
    assert!(dependency_root.join("lib.harn").exists());
}

#[test]
fn add_github_shorthand_requires_version_or_ref() {
    let error = normalize_add_request(
        "github.com/burin-labs/harn-openapi",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap_err();
    assert!(error
        .to_string()
        .contains("must specify `tag`, `rev`, or `branch`"));
}

#[test]
fn add_github_shorthand_with_ref_writes_git_dependency() {
    let (alias, dependency) = normalize_add_request(
        "github.com/burin-labs/harn-openapi@v1.2.3",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(alias, "harn-openapi");
    let item = dependency_manifest_item(&alias, &dependency).unwrap();
    let table = item.as_inline_table().unwrap();
    assert_eq!(
        table.get("git").and_then(toml_edit::Value::as_str),
        Some("https://github.com/burin-labs/harn-openapi")
    );
    assert_eq!(
        table.get("rev").and_then(toml_edit::Value::as_str),
        Some("v1.2.3")
    );
}

#[test]
fn package_alias_validation_rejects_path_traversal_names() {
    for alias in [
        "../evil",
        "nested/evil",
        "nested\\evil",
        ".",
        "..",
        "bad alias",
    ] {
        assert!(
            validate_package_alias(alias).is_err(),
            "{alias:?} should be rejected"
        );
    }
    validate_package_alias("acme-lib_1.2").expect("ordinary alias should be accepted");
}

#[test]
fn add_package_rejects_aliases_that_escape_packages_dir() {
    let error = normalize_add_request(
        "ignored",
        Some("../evil"),
        None,
        None,
        None,
        None,
        Some("./dep"),
        None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("invalid dependency alias"));
}

#[test]
fn rendered_dependency_values_are_toml_escaped() {
    let path = "dep\" \nmalicious = true";
    let item = dependency_manifest_item(
        "safe",
        &Dependency::Table(Box::new(DepTable {
            path: Some(path.to_string()),
            ..DepTable::default()
        })),
    )
    .expect("dependency item");
    let mut document = toml_edit::DocumentMut::new();
    document["dependencies"] = toml_edit::Item::Table(toml_edit::Table::new());
    document["dependencies"]["safe"] = item;
    let parsed: Manifest = toml::from_str(&document.to_string()).unwrap();
    assert_eq!(parsed.dependencies.len(), 1);
    assert_eq!(
        parsed
            .dependencies
            .get("safe")
            .and_then(Dependency::local_path),
        Some(path)
    );
}

#[test]
fn dependency_edits_preserve_unrelated_formatting_and_comments() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest_path = tmp.path().join(MANIFEST);
    fs::write(
        &manifest_path,
        "# project\n[dependencies] # managed here\n\"acme.lib\" = { path = \"old\" } # retain\n\n[tool]\ncustom = true\n",
    )
    .unwrap();

    upsert_dependency_in_manifest_locked(
        &manifest_path,
        "acme.lib",
        &Dependency::Table(Box::new(DepTable {
            path: Some("new".to_string()),
            ..DepTable::default()
        })),
    )
    .unwrap();

    let updated = fs::read_to_string(&manifest_path).unwrap();
    assert!(updated.starts_with("# project\n[dependencies] # managed here\n"));
    assert!(updated.contains("\"acme.lib\" = { path = \"new\" } # retain"));
    assert!(updated.ends_with("\n[tool]\ncustom = true\n"));
    assert!(remove_dependency_from_manifest_locked(&manifest_path, "acme.lib").unwrap());
    let removed = fs::read_to_string(&manifest_path).unwrap();
    assert!(removed.starts_with("# project\n[dependencies] # managed here\n"));
    assert!(removed.ends_with("\n[tool]\ncustom = true\n"));
}
