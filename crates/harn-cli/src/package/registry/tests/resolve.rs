//! Registry search, info, and dependency synthesis.

use crate::package::test_support::*;
use crate::package::*;

#[test]
fn registry_index_search_and_info_use_local_file_without_network() {
    let (_repo_tmp, repo, _branch) = create_git_package_repo();
    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let workspace = TestWorkspace::new(root);
    let registry_path = root.join("index.toml");
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    write_package_registry_index(&registry_path, "@burin/acme-lib", &git, "acme-lib");
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

    let matches = search_package_registry_in(
        workspace.env(),
        Some("acme"),
        Some(registry_path.to_string_lossy().as_ref()),
    )
    .unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].name, "@burin/acme-lib");
    assert_eq!(
        matches[0].harn.as_deref(),
        Some(crate::package::current_harn_range_example().as_str())
    );
    assert_eq!(matches[0].connector_contract.as_deref(), Some("v1"));
    assert_eq!(matches[0].exports, vec!["lib"]);

    let info = package_registry_info_in(
        workspace.env(),
        "@burin/acme-lib@1.0.0",
        Some(registry_path.to_string_lossy().as_ref()),
    )
    .unwrap();
    assert_eq!(info.package.license.as_deref(), Some("MIT OR Apache-2.0"));
    assert_eq!(
        info.selected_version
            .as_ref()
            .and_then(|version| version.git.as_deref()),
        Some(git.as_str())
    );
}

#[test]
fn rule_registry_search_filters_to_rule_pack_metadata() {
    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let workspace = TestWorkspace::new(root);
    let registry_path = root.join("index.toml");
    fs::write(
        &registry_path,
        r#"
version = 1

[[package]]
name = "@acme/plain"
description = "Plain package"
repository = "https://github.com/acme/plain"

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/plain"
tag = "v1.0.0"

[[package]]
name = "@acme/rules"
description = "TypeScript and Rust cleanup rules"
repository = "https://github.com/acme/rules"

[package.rule_pack]
rule_count = 3
languages = ["typescript", "rust", "typescript"]
safety_summary = ["no-fix:1", "behavior-preserving:2"]

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/rules"
tag = "v1.0.0"
"#,
    )
    .unwrap();
    fs::write(root.join(MANIFEST), "[package]\nname = \"workspace\"\n").unwrap();

    let matches = search_rule_package_registry_in(
        workspace.env(),
        Some("rust"),
        Some(registry_path.to_string_lossy().as_ref()),
    )
    .unwrap();

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].name, "@acme/rules");
    let metadata = matches[0].rule_pack.as_ref().expect("rule pack metadata");
    assert_eq!(metadata.rule_count, 3);
    assert_eq!(metadata.languages, vec!["rust", "typescript"]);
    assert_eq!(
        metadata.safety_summary,
        vec!["behavior-preserving:2", "no-fix:1"]
    );
}

#[test]
fn add_registry_dependency_preserves_provenance_in_manifest_and_lock() {
    let (_repo_tmp, repo, _branch) = create_git_package_repo();
    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let registry_path = root.join("index.toml");
    let workspace =
        TestWorkspace::new(root).with_registry_source(registry_path.display().to_string());
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    write_package_registry_index(&registry_path, "@burin/acme-lib", &git, "acme-lib");
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
        "@burin/acme-lib@1.0.0",
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
        manifest.contains(&format!("git = \"{git}\"")),
        "registry install must record the resolved git URL: {manifest}"
    );
    assert!(
        manifest.contains("tag = \"v1.0.0\""),
        "registry install must pin the resolved tag: {manifest}"
    );
    assert!(
        manifest.contains("registry_name = \"@burin/acme-lib\""),
        "registry install must preserve the registry-side package name: {manifest}"
    );
    assert!(
        manifest.contains("registry_version = \"1.0.0\""),
        "registry install must preserve the requested registry version: {manifest}"
    );
    let lock = LockFile::load(&root.join(LOCK_FILE)).unwrap().unwrap();
    let entry = lock.find("acme-lib").unwrap();
    assert_eq!(entry.source, format!("git+{git}"));
    let registry = entry
        .registry
        .as_ref()
        .expect("registry-added entry should carry registry provenance");
    assert_eq!(registry.name, "@burin/acme-lib");
    assert_eq!(registry.version, "1.0.0");
    assert!(current_packages_dir(root)
        .join("acme-lib")
        .join("lib.harn")
        .is_file());
}

#[test]
fn add_registry_dependency_accepts_bare_alias_and_semver_range() {
    // Covers the literal acceptance from the free-tier package-manager
    // epic (harn#2157): `harn add notion-sdk-harn@^0.1` should resolve
    // even though the registry-side name is `@burin/notion-sdk`.
    let (_repo_tmp, repo, _branch) = create_git_package_repo();
    let project_tmp = tempfile::tempdir().unwrap();
    let root = project_tmp.path();
    let registry_path = root.join("index.toml");
    let workspace =
        TestWorkspace::new(root).with_registry_source(registry_path.display().to_string());
    let git = normalize_git_url(repo.to_string_lossy().as_ref()).unwrap();
    write_package_registry_index(&registry_path, "@burin/acme-lib", &git, "acme-lib");
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

    // Bare alias + semver range. Highest matching unyanked version wins.
    add_package_to(
        workspace.env(),
        "acme-lib@^1",
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
        manifest.contains("registry_name = \"@burin/acme-lib\""),
        "bare-alias add must record the canonical scoped registry name: {manifest}"
    );
    assert!(
        manifest.contains("registry_version = \"1.0.0\""),
        "semver range must resolve to the highest matching exact version: {manifest}"
    );
}
