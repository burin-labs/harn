use super::*;
use crate::package::test_support::*;

#[test]
fn package_check_accepts_publishable_package() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());

    let report = check_package_impl(Some(tmp.path())).unwrap();

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.name.as_deref(), Some("acme-lib"));
    assert_eq!(report.exports[0].symbols[0].name, "greet");
}

#[test]
fn package_check_rejects_path_dependencies_and_bad_harn_range() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());
    fs::write(
        tmp.path().join(MANIFEST),
        r#"[package]
name = "acme-lib"
version = "0.1.0"
description = "Acme helpers"
license = "MIT"
repository = "https://github.com/acme/acme-lib"
harn = ">=999.0,<999.1"
docs_url = "docs/api.md"

[exports]
lib = "lib/main.harn"

[dependencies]
local = { path = "../local" }
"#,
    )
    .unwrap();

    let report = check_package_impl(Some(tmp.path())).unwrap();
    let messages = report
        .errors
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(messages.contains("unsupported Harn version range"));
    assert!(messages.contains("path dependencies are not publishable"));
}

#[test]
fn package_check_warns_on_branch_dependency() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());
    fs::write(
        tmp.path().join(MANIFEST),
        format!(
            r#"[package]
name = "acme-lib"
version = "0.1.0"
description = "Acme helpers"
license = "MIT"
repository = "https://github.com/acme/acme-lib"
harn = "{}"
docs_url = "docs/api.md"

[exports]
lib = "lib/main.harn"

[dependencies]
remote = {{ git = "https://github.com/acme/remote-lib", branch = "main" }}
"#,
            current_harn_range_example()
        ),
    )
    .unwrap();

    let report = check_package_impl(Some(tmp.path())).unwrap();
    let warnings = report
        .warnings
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(warnings.contains("branch dependencies are non-reproducible"));
}

#[test]
fn extract_api_symbols_recognizes_block_doc_comments() {
    // `/** … */` (the canonical HarnDoc form preferred by the linter)
    // and `///` lines must produce the same `docs` body so package
    // check, package docs, and the missing-doc warning agree on what
    // counts as documented.
    let single = extract_api_symbols("/** Block doc. */\npub fn one() {}\n");
    assert_eq!(single.len(), 1);
    assert_eq!(single[0].docs.as_deref(), Some("Block doc."));

    let multi = extract_api_symbols("/**\n * First line.\n * Second line.\n */\npub fn two() {}\n");
    assert_eq!(multi.len(), 1);
    assert_eq!(multi[0].docs.as_deref(), Some("First line.\nSecond line."));

    let triple = extract_api_symbols("/// Slash doc.\npub fn three() {}\n");
    assert_eq!(triple.len(), 1);
    assert_eq!(triple[0].docs.as_deref(), Some("Slash doc."));

    // A non-doc, non-empty intermediate line clears the pending
    // doc buffer so an unrelated comment three lines up does not
    // accidentally bind to the declaration.
    let detached = extract_api_symbols("/** Detached. */\nlet x = 1\npub fn four() {}\n");
    assert_eq!(detached.len(), 1);
    assert!(detached[0].docs.is_none());
}

#[test]
fn package_docs_and_pack_use_exports() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());

    let docs_path = generate_package_docs_impl(Some(tmp.path()), None, false).unwrap();
    let docs = fs::read_to_string(docs_path).unwrap();
    assert!(docs.contains("### fn `greet`"));
    assert!(docs.contains("Return a greeting."));

    let pack = pack_package_impl(Some(tmp.path()), None, true).unwrap();
    assert!(pack.files.contains(&"harn.toml".to_string()));
    assert!(pack.files.contains(&"lib/main.harn".to_string()));
}

#[test]
fn package_pack_skips_generated_docs_dist() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());
    fs::create_dir_all(tmp.path().join("docs/dist")).unwrap();
    fs::write(tmp.path().join("docs/dist/index.html"), "<html></html>\n").unwrap();

    let pack = pack_package_impl(Some(tmp.path()), None, true).unwrap();

    assert!(
        !pack.files.iter().any(|path| path.starts_with("docs/dist/")),
        "{:?}",
        pack.files
    );
}

#[test]
fn publish_dry_run_builds_tag_command_and_index_diff() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());
    write_release_changelog(tmp.path(), "0.1.0");
    let _remote = init_publishable_repo(tmp.path());
    let index = r#"version = 1

[[package]]
name = "acme-lib"
repository = "https://github.com/acme/acme-lib"

[[package.version]]
version = "0.0.1"
git = "https://github.com/acme/acme-lib"
rev = "deadbeef"

[[package]]
name = "other-lib"
repository = "https://github.com/acme/other-lib"

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/other-lib"
rev = "feedface"
"#;
    let index_path = Path::new("package-index/harn-package-index.toml");
    let options = PackagePublishOptions {
        dry_run: true,
        remote: "origin",
        index_repo: "burin-labs/harn-cloud",
        index_path,
        registry_name: None,
        skip_index_pr: false,
        registry: None,
    };

    let plan = prepare_publish_plan(
        Some(tmp.path()),
        &options,
        index.to_string(),
        "fixture",
        None,
    )
    .unwrap();

    assert!(plan.tag_command.contains("git -C"));
    assert!(plan.tag_command.contains("tag v0.1.0"));
    assert!(plan.index_diff.contains("+version = \"0.1.0\""));
    assert!(plan.index_diff.contains("+tag = \"v0.1.0\""));
    assert!(plan
        .index_diff
        .contains(&format!("+rev = \"{}\"", plan.sha)));
    assert!(plan
        .index_diff
        .contains(&format!("+sha = \"{}\"", plan.sha)));
    let acme_pos = plan
        .updated_index_content
        .find("name = \"acme-lib\"")
        .unwrap();
    let other_pos = plan
        .updated_index_content
        .find("name = \"other-lib\"")
        .unwrap();
    let new_version_pos = plan
        .updated_index_content
        .find("version = \"0.1.0\"")
        .unwrap();
    assert!(acme_pos < new_version_pos && new_version_pos < other_pos);
}

#[test]
fn rule_publish_marks_pure_rule_pack_in_index() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_rule_pack(tmp.path());
    write_release_changelog(tmp.path(), "0.1.0");
    let _remote = init_publishable_repo(tmp.path());
    let index_path = Path::new("package-index/harn-package-index.toml");
    let options = PackagePublishOptions {
        dry_run: true,
        remote: "origin",
        index_repo: "burin-labs/harn-cloud",
        index_path,
        registry_name: Some("@acme/rules"),
        skip_index_pr: false,
        registry: None,
    };
    let rule_pack =
        collect_rule_pack_metadata(&load_manifest_context_for_anchor(Some(tmp.path())).unwrap())
            .unwrap()
            .expect("rule pack metadata");

    let plan = prepare_publish_plan(
        Some(tmp.path()),
        &options,
        "version = 1\n".to_string(),
        "fixture",
        Some(rule_pack),
    )
    .unwrap();

    assert!(plan.updated_index_content.contains("[package.rule_pack]"));
    assert!(plan.updated_index_content.contains("rule_count = 2"));
    assert!(plan
        .updated_index_content
        .contains("languages = [\"typescript\"]"));
    assert!(plan
        .updated_index_content
        .contains("safety_summary = [\"behavior-preserving:1\", \"no-fix:1\"]"));
    let registry_path = tmp.path().join("index.toml");
    fs::write(&registry_path, &plan.updated_index_content).unwrap();
    let workspace = TestWorkspace::new(tmp.path());
    let matches = search_rule_package_registry_in(
        workspace.env(),
        Some("typescript"),
        Some(registry_path.to_string_lossy().as_ref()),
    )
    .unwrap();
    assert_eq!(matches.len(), 1);
    let package = serde_json::to_value(&matches[0]).unwrap();
    assert_eq!(package["name"], "@acme/rules");
    assert_eq!(package["rule_pack"]["rule_count"], 2);
    assert!(plan.index_diff.contains("+[package.rule_pack]"));
}

#[test]
fn rule_pack_metadata_upsert_marks_existing_package_block() {
    let content = r#"version = 1

[[package]]
name = "@acme/rules"
repository = "https://github.com/acme/rules"

[[package.version]]
version = "0.1.0"
git = "https://github.com/acme/rules"
tag = "v0.1.0"
"#;
    let metadata = RegistryRulePackInfo {
        rule_count: 1,
        languages: vec!["typescript".to_string()],
        safety_summary: vec!["no-fix:1".to_string()],
    };

    let updated = upsert_rule_pack_metadata(content, "@acme/rules", &metadata).unwrap();

    let marker = updated.find("[package.rule_pack]").unwrap();
    let version = updated.find("[[package.version]]").unwrap();
    assert!(marker < version, "{updated}");
    parse_package_registry_index("fixture", &updated).unwrap();
}

#[test]
fn publish_preflight_rejects_existing_tag_and_missing_changelog_entry() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());
    let _remote = init_publishable_repo(tmp.path());
    let index_path = Path::new("package-index/harn-package-index.toml");
    let options = PackagePublishOptions {
        dry_run: true,
        remote: "origin",
        index_repo: "burin-labs/harn-cloud",
        index_path,
        registry_name: None,
        skip_index_pr: false,
        registry: None,
    };

    let missing_changelog = prepare_publish_plan(
        Some(tmp.path()),
        &options,
        "version = 1\n".to_string(),
        "fixture",
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(missing_changelog.contains("CHANGELOG.md"));

    write_release_changelog(tmp.path(), "0.1.0");
    run_git(tmp.path(), &["add", "CHANGELOG.md"]);
    run_git(tmp.path(), &["commit", "-m", "add changelog"]);
    run_git(tmp.path(), &["tag", "v0.1.0"]);

    let existing_tag = prepare_publish_plan(
        Some(tmp.path()),
        &options,
        "version = 1\n".to_string(),
        "fixture",
        None,
    )
    .unwrap_err()
    .to_string();
    assert!(existing_tag.contains("already exists locally"));
}

#[test]
fn publish_preflight_rejects_dirty_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());
    write_release_changelog(tmp.path(), "0.1.0");
    let _remote = init_publishable_repo(tmp.path());
    fs::write(tmp.path().join("scratch.txt"), "dirty\n").unwrap();
    let index_path = Path::new("package-index/harn-package-index.toml");
    let options = PackagePublishOptions {
        dry_run: true,
        remote: "origin",
        index_repo: "burin-labs/harn-cloud",
        index_path,
        registry_name: None,
        skip_index_pr: false,
        registry: None,
    };

    let error = prepare_publish_plan(
        Some(tmp.path()),
        &options,
        "version = 1\n".to_string(),
        "fixture",
        None,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("working tree must be clean"));
    assert!(error.contains("scratch.txt"));
}

#[cfg(unix)]
#[test]
fn package_pack_does_not_follow_symlinked_files() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());
    let outside = tempfile::NamedTempFile::new().unwrap();
    fs::write(outside.path(), "secret\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), tmp.path().join("secret.txt")).unwrap();

    let pack = pack_package_impl(Some(tmp.path()), None, true).unwrap();

    assert!(
        !pack.files.contains(&"secret.txt".to_string()),
        "{:?}",
        pack.files
    );
}

#[test]
fn package_relative_paths_reject_windows_rooted_forms() {
    let tmp = tempfile::tempdir().unwrap();
    for rel_path in [
        "/repo/secret.harn",
        r"\repo\secret.harn",
        r"C:\repo\secret.harn",
        "C:secret.harn",
        r"\\server\share\secret.harn",
        r"..\secret.harn",
        r"lib\..\secret.harn",
        r"lib/..\secret.harn",
    ] {
        assert!(
            safe_package_relative_path(tmp.path(), rel_path).is_err(),
            "{rel_path:?} must not be accepted as package-relative"
        );
    }
}

#[test]
fn package_check_validates_tool_and_skill_exports() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());
    fs::create_dir_all(tmp.path().join("skills/review")).unwrap();
    fs::write(
        tmp.path().join("harn.toml"),
        format!(
            r#"[package]
name = "acme-lib"
version = "0.1.0"
description = "Acme helpers"
license = "MIT"
repository = "https://github.com/acme/acme-lib"
harn = "{}"
docs_url = "docs/api.md"
permissions = ["tool:read_only"]
host_requirements = ["workspace.read_text"]

[exports]
lib = "lib/main.harn"

[[package.tools]]
name = "read-note"
module = "lib/main.harn"
symbol = "tools"
permissions = ["tool:read_only"]

[package.tools.input_schema]
type = "object"
required = ["path"]

[package.tools.annotations]
kind = "read"
side_effect_level = "read_only"

[package.tools.annotations.arg_schema]
required = ["path"]

[[package.skills]]
name = "review"
path = "skills/review"
permissions = ["skill:prompt"]

[dependencies]
"#,
            current_harn_range_example()
        ),
    )
    .unwrap();
    fs::write(
        tmp.path().join("skills/review/SKILL.md"),
        "---\nname: review\nshort: Review changes\n---\n# Review\n",
    )
    .unwrap();

    let report = check_package_impl(Some(tmp.path())).unwrap();

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(report.tools[0].name, "read-note");
    assert_eq!(
        report.tools[0].host_requirements,
        vec!["workspace.read_text"]
    );
    assert_eq!(report.skills[0].name, "review");
}

#[test]
fn package_check_rejects_invalid_tool_schema_and_host_requirement() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());
    fs::write(
        tmp.path().join(MANIFEST),
        format!(
            r#"[package]
name = "acme-lib"
version = "0.1.0"
description = "Acme helpers"
license = "MIT"
repository = "https://github.com/acme/acme-lib"
harn = "{}"
docs_url = "docs/api.md"

[exports]
lib = "lib/main.harn"

[[package.tools]]
name = "broken"
module = "lib/main.harn"
symbol = "tools"
host_requirements = ["workspace"]

[package.tools.input_schema]
required = [1]

[dependencies]
"#,
            current_harn_range_example()
        ),
    )
    .unwrap();

    let report = check_package_impl(Some(tmp.path())).unwrap();
    let messages = report
        .errors
        .iter()
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(messages.contains("capability.operation"));
    assert!(messages.contains("schema `required` must be a list of strings"));
}

#[test]
fn package_doctor_accepts_application_manifests_with_tool_exports() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join(MANIFEST),
        r#"[package]
name = "acme-app"

[[package.tools]]
name = "echo"
module = "tools.harn"
symbol = "tools"

[package.tools.input_schema]
type = "object"

[package.tools.annotations]
kind = "read"
side_effect_level = "read_only"
"#,
    )
    .unwrap();
    fs::write(tmp.path().join("tools.harn"), "pub fn tools() {}\n").unwrap();
    let workspace = TestWorkspace::new(tmp.path());

    let report = doctor_packages_in(workspace.env()).unwrap();

    assert!(report.ok, "{:?}", report.diagnostics);
    assert!(
        report
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "root-package-check"),
        "{:?}",
        report.diagnostics
    );
}

#[test]
fn package_list_reports_locked_tool_and_skill_exports() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join(MANIFEST),
        r#"[package]
name = "consumer"
"#,
    )
    .unwrap();
    let lock = LockFile {
        packages: vec![LockEntry {
            name: "acme-tools".to_string(),
            source: "path+../acme-tools".to_string(),
            package_version: Some("0.1.0".to_string()),
            provenance: Some("https://github.com/acme/acme-tools/releases/tag/v0.1.0".to_string()),
            exports: PackageLockExports {
                modules: vec![PackageLockExport {
                    name: "tools".to_string(),
                    path: Some("lib/tools.harn".to_string()),
                    symbol: None,
                }],
                tools: vec![PackageLockExport {
                    name: "echo".to_string(),
                    path: Some("lib/tools.harn".to_string()),
                    symbol: Some("tools".to_string()),
                }],
                skills: vec![PackageLockExport {
                    name: "review".to_string(),
                    path: Some("skills/review".to_string()),
                    symbol: None,
                }],
                personas: Vec::new(),
            },
            permissions: vec!["tool:read_only".to_string()],
            host_requirements: vec!["workspace.read_text".to_string()],
            ..LockEntry::default()
        }],
        ..LockFile::default()
    };
    let lock_body = toml::to_string_pretty(&lock).unwrap();
    fs::write(tmp.path().join(LOCK_FILE), lock_body).unwrap();
    let workspace = TestWorkspace::new(tmp.path());

    let report = list_packages_in(workspace.env()).unwrap();

    assert_eq!(report.packages.len(), 1);
    let package = &report.packages[0];
    assert_eq!(package.name, "acme-tools");
    assert_eq!(
        package.provenance.as_deref(),
        Some("https://github.com/acme/acme-tools/releases/tag/v0.1.0")
    );
    assert_eq!(package.exports.tools[0].name, "echo");
    assert_eq!(package.exports.skills[0].name, "review");
    assert_eq!(package.permissions, vec!["tool:read_only"]);
    assert_eq!(package.host_requirements, vec!["workspace.read_text"]);
}

fn write_release_changelog(root: &Path, version: &str) {
    fs::write(
        root.join("CHANGELOG.md"),
        format!("# Changelog\n\n## {version}\n\n- Initial release.\n"),
    )
    .unwrap();
}

fn write_publishable_rule_pack(root: &Path) {
    fs::create_dir_all(root.join("rules")).unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    let harn_range = current_harn_range_example();
    fs::write(
        root.join(MANIFEST),
        format!(
            r#"[package]
name = "acme-rules"
version = "0.1.0"
description = "Acme structural rules"
license = "MIT"
repository = "https://github.com/acme/acme-rules"
harn = "{harn_range}"
docs_url = "docs/api.md"

[rules]
ruleDirs = ["rules"]

[dependencies]
"#
        ),
    )
    .unwrap();
    fs::write(
        root.join("rules/no-foo.toml"),
        "id = \"no-foo\"\nlanguage = \"typescript\"\nmessage = \"no foo\"\n[rule]\npattern = \"foo()\"\n",
    )
    .unwrap();
    fs::write(
        root.join("rules/rename.toml"),
        "id = \"rename\"\nlanguage = \"typescript\"\nfix = \"bar()\"\nsafety = \"behavior-preserving\"\n[rule]\npattern = \"foo()\"\n",
    )
    .unwrap();
    fs::write(root.join("README.md"), "# acme-rules\n").unwrap();
    fs::write(root.join("LICENSE"), "MIT\n").unwrap();
    fs::write(root.join("docs/api.md"), "").unwrap();
}

fn init_publishable_repo(root: &Path) -> tempfile::TempDir {
    let init = test_git_command(root)
        .args(["init", "-b", "main"])
        .output()
        .unwrap();
    if !init.status.success() {
        run_git(root, &["init"]);
    }
    run_git(root, &["config", "user.email", "tests@example.com"]);
    run_git(root, &["config", "user.name", "Harn Tests"]);
    run_git(root, &["config", "core.hooksPath", "/dev/null"]);
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-m", "initial"]);

    let remote = tempfile::tempdir().unwrap();
    let bare = remote.path().join("origin.git");
    let output = test_git_command(root)
        .args(["init", "--bare", bare.to_string_lossy().as_ref()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git init --bare failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    run_git(
        root,
        &["remote", "add", "origin", bare.to_string_lossy().as_ref()],
    );
    remote
}
