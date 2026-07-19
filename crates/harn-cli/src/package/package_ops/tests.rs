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
fn package_check_resolves_transitive_public_re_exports() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());
    fs::write(
        tmp.path().join("lib/main.harn"),
        r#"/** Local package entry. */
pub fn local() -> string { return "local" }

pub import { Config, greet } from "./middle"
pub import "./middle"
"#,
    )
    .unwrap();
    fs::write(
        tmp.path().join("lib/middle.harn"),
        "pub import \"./implementation\"\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("lib/implementation.harn"),
        r#"/** Name passed to greet. */
pub type Config = string

/** Return a greeting from the implementation module. */
pub fn greet(name: Config) -> string { return "hi " + name }
"#,
    )
    .unwrap();

    let report = check_package_impl(Some(tmp.path())).unwrap();

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let symbols = &report.exports[0].symbols;
    assert_eq!(
        symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>(),
        vec!["local", "Config", "greet"]
    );
    assert_eq!(symbols[1].docs.as_deref(), Some("Name passed to greet."));
    assert_eq!(symbols[1].signature, "pub type Config = string");
    assert_eq!(
        symbols[2].docs.as_deref(),
        Some("Return a greeting from the implementation module.")
    );
    assert_eq!(symbols[2].signature, "pub fn greet(name: Config) -> string");
    assert!(!report
        .warnings
        .iter()
        .any(|warning| warning.message.contains("no public symbols")));
}

#[test]
fn package_docs_render_forwarded_origin_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());
    fs::write(
        tmp.path().join("lib/main.harn"),
        "pub import { greet } from \"./implementation\"\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("lib/implementation.harn"),
        "/** Return the implementation greeting. */\npub fn greet(name: string) -> string { return \"hi \" + name }\n",
    )
    .unwrap();

    let docs_path = generate_package_docs_impl(Some(tmp.path()), None, false).unwrap();
    let docs = fs::read_to_string(docs_path).unwrap();

    assert!(docs.contains("### fn `greet`"), "{docs}");
    assert!(
        docs.contains("Return the implementation greeting."),
        "{docs}"
    );
    assert!(
        docs.contains("pub fn greet(name: string) -> string"),
        "{docs}"
    );
}

#[test]
fn package_check_reports_public_re_export_conflicts() {
    let tmp = tempfile::tempdir().unwrap();
    write_publishable_package(tmp.path());
    fs::write(
        tmp.path().join("lib/main.harn"),
        "pub import { shared } from \"./a\"\npub import { shared } from \"./b\"\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("lib/a.harn"),
        "/** First definition. */\npub fn shared() -> string { return \"a\" }\n",
    )
    .unwrap();
    fs::write(
        tmp.path().join("lib/b.harn"),
        "/** Second definition. */\npub fn shared() -> string { return \"b\" }\n",
    )
    .unwrap();

    let report = check_package_impl(Some(tmp.path())).unwrap();
    let errors = report
        .errors
        .iter()
        .map(|error| error.message.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(errors.contains("re-export conflict"), "{errors}");
    assert!(errors.contains("'shared'"), "{errors}");
}

#[test]
fn package_check_accepts_contribution_only_package() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("docs")).unwrap();
    fs::write(
        tmp.path().join(MANIFEST),
        format!(
            r#"[package]
name = "harn-canon"
version = "0.1.0"
description = "Canon packs"
license = "MIT"
repository = "https://github.com/burin-labs/harn-canon"
harn = "{}"
docs_url = "docs/api.md"

[[contributes]]
kind = "harn.canon"
id = "harn-canon"
title = "Harn Canon"
manifest = "canon-packs.json"
"#,
            current_harn_range_example()
        ),
    )
    .unwrap();
    fs::write(tmp.path().join("README.md"), "# harn-canon\n").unwrap();
    fs::write(tmp.path().join("LICENSE"), "MIT\n").unwrap();
    fs::write(tmp.path().join("docs/api.md"), "").unwrap();
    fs::write(
        tmp.path().join("canon-packs.json"),
        r#"{"schema_version":1,"packs":[]}"#,
    )
    .unwrap();

    let report = check_package_impl(Some(tmp.path())).unwrap();

    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert!(report.exports.is_empty());
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
fn extract_api_symbols_ignores_declarations_inside_strings() {
    let symbols = extract_api_symbols(
        r#"/** Real export. */
pub fn real() {}

const generated = """
/** Generated code, not an export. */
pub fn generated() {}
"""
"#,
    );

    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].name, "real");
    assert_eq!(symbols[0].docs.as_deref(), Some("Real export."));
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
fn package_pack_excludes_linked_worktree_and_nested_harn_runtime_state() {
    let package = tempfile::tempdir().unwrap();
    let artifact_parent = tempfile::tempdir().unwrap();
    let artifact = artifact_parent.path().join("artifact");
    write_publishable_package(package.path());
    fs::write(
        package.path().join(".git"),
        "gitdir: ../worktrees/package\n",
    )
    .unwrap();
    fs::write(package.path().join(".harn-version"), "0.10.0\n").unwrap();
    fs::create_dir_all(package.path().join("tests/.harn-tmp/case")).unwrap();
    fs::write(
        package.path().join("tests/.harn-tmp/case/runtime.txt"),
        "transient\n",
    )
    .unwrap();

    let pack = pack_package_impl(Some(package.path()), Some(&artifact), false).unwrap();

    assert!(pack.files.contains(&".harn-version".to_string()));
    assert!(!pack.files.iter().any(|path| path == ".git"));
    assert!(!pack.files.iter().any(|path| path.contains(".harn-tmp")));
    assert!(artifact.join(".harn-version").is_file());
    assert!(!artifact.join(".git").exists());
    assert!(!artifact.join("tests/.harn-tmp").exists());
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
    let index_path = Path::new("harn-package-index.toml");
    let options = PackagePublishOptions {
        dry_run: true,
        remote: "origin",
        index_repo: "burin-labs/harn-packages",
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
    let index_path = Path::new("harn-package-index.toml");
    let options = PackagePublishOptions {
        dry_run: true,
        remote: "origin",
        index_repo: "burin-labs/harn-packages",
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
    let index_path = Path::new("harn-package-index.toml");
    let options = PackagePublishOptions {
        dry_run: true,
        remote: "origin",
        index_repo: "burin-labs/harn-packages",
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
    let index_path = Path::new("harn-package-index.toml");
    let options = PackagePublishOptions {
        dry_run: true,
        remote: "origin",
        index_repo: "burin-labs/harn-packages",
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

#[test]
fn package_check_reports_personas_and_rejects_missing_entry_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join(MANIFEST),
        format!(
            "[package]\nname = \"agents\"\nversion = \"1.0.0\"\n\n{}",
            persona_manifest("reviewer", "missing.harn#run")
        ),
    )
    .unwrap();

    let report = check_package_impl(Some(tmp.path())).unwrap();

    assert!(report.personas.is_empty());
    assert!(
        report.errors.iter().any(|error| {
            error.field == "[[personas]].entry_workflow" && error.message.contains("missing.harn")
        }),
        "{:?}",
        report.errors
    );

    fs::write(
        tmp.path().join("missing.harn"),
        "pub pipeline run(task) -> dict { return {ok: true} }\n",
    )
    .unwrap();
    let report = check_package_impl(Some(tmp.path())).unwrap();
    assert_eq!(report.personas.len(), 1);
    assert_eq!(report.personas[0].name, "reviewer");
    assert_eq!(report.personas[0].entry_workflow, "missing.harn#run");
}

#[tokio::test]
async fn installed_persona_catalog_is_qualified_sorted_and_directly_resolvable() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join(MANIFEST),
        format!(
            "[package]\nname = \"consumer\"\n\n[dependencies]\nagents = {{ path = \"vendor/agents\" }}\n\n{}",
            persona_manifest("reviewer", "root.harn#run")
        ),
    )
    .unwrap();
    fs::write(
        tmp.path().join("root.harn"),
        "pub pipeline run(task) -> dict { return {root: true} }\n",
    )
    .unwrap();
    let mut lock = install_test_persona_package(
        tmp.path(),
        "agents",
        vec!["reviewer".to_string(), "archivist".to_string()],
        &["reviewer", "archivist"],
    );
    let dependency_source = tmp.path().join("vendor/agents");
    fs::create_dir_all(&dependency_source).unwrap();
    let installed_package = current_packages_dir(tmp.path()).join("agents");
    fs::copy(
        installed_package.join(MANIFEST),
        dependency_source.join(MANIFEST),
    )
    .unwrap();
    fs::copy(
        installed_package.join("workflow.harn"),
        dependency_source.join("workflow.harn"),
    )
    .unwrap();
    lock.packages[0].source = path_source_uri(&dependency_source.canonicalize().unwrap()).unwrap();
    let lock_body = toml::to_string_pretty(&lock).unwrap();
    fs::write(tmp.path().join(LOCK_FILE), &lock_body).unwrap();
    write_test_generation_lock(tmp.path(), &lock_body);

    let personas = load_discoverable_personas(Some(&tmp.path().join(MANIFEST))).unwrap();
    let ids = personas
        .iter()
        .map(|persona| persona.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["agents/archivist", "agents/reviewer", "reviewer"]);

    let root = resolve_discoverable_persona(Some(&tmp.path().join(MANIFEST)), "reviewer").unwrap();
    assert_eq!(root.id, "reviewer");
    assert!(root.installed_provenance().is_none());

    let installed =
        resolve_discoverable_persona(Some(&tmp.path().join(MANIFEST)), "agents/reviewer").unwrap();
    let provenance = installed.installed_provenance().unwrap();
    assert_eq!(provenance.package_alias, "agents");
    assert_eq!(provenance.package_version.as_deref(), Some("1.2.3"));
    assert!(provenance
        .content_hash
        .as_deref()
        .unwrap()
        .starts_with("sha256:"));

    let payload = crate::commands::persona::list_payload(Some(&tmp.path().join(MANIFEST))).unwrap();
    assert_eq!(payload[0]["id"], "agents/archivist");
    assert_eq!(payload[0]["source"]["package_alias"], "agents");
    assert_eq!(payload[0]["source"]["kind"], "installed_package");
    assert_eq!(payload[0]["source"]["package_version"], "1.2.3");
    assert!(payload[0]["source"]["content_hash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(payload[0]["source"]["integrity"], "ok");
    let inspect = crate::commands::persona::inspect_payload(
        Some(&tmp.path().join(MANIFEST)),
        "agents/reviewer",
    )
    .unwrap();
    assert_eq!(inspect["id"], "agents/reviewer");
    assert_eq!(inspect["name"], "reviewer");

    let status_error = crate::commands::persona::status_payload(
        Some(&tmp.path().join(MANIFEST)),
        &tmp.path().join("state"),
        "agents/reviewer",
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(
        status_error,
        "active runtime persona 'agents/reviewer' not found"
    );

    let workspace = TestWorkspace::new(tmp.path());
    let report = list_packages_in(workspace.env()).unwrap();
    assert_eq!(report.personas.len(), 2);
    assert_eq!(report.personas[0].id, "agents/archivist");
    assert_eq!(report.personas[1].id, "agents/reviewer");
}

#[test]
fn dependency_persona_collisions_require_qualification_and_direct_inspect_is_targeted() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join(MANIFEST),
        "[package]\nname = \"consumer\"\n",
    )
    .unwrap();
    create_test_package_generation(tmp.path());
    let mut lock = LockFile::default();
    add_test_persona_package(
        tmp.path(),
        &mut lock,
        "zeta",
        vec!["reviewer".to_string()],
        &["reviewer"],
    );
    add_test_persona_package(
        tmp.path(),
        &mut lock,
        "alpha",
        vec!["reviewer".to_string()],
        &["reviewer"],
    );

    let manifest = tmp.path().join(MANIFEST);
    let personas = load_discoverable_personas(Some(&manifest)).unwrap();
    assert_eq!(
        personas
            .iter()
            .map(|persona| persona.id.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha/reviewer", "zeta/reviewer"]
    );
    assert!(resolve_discoverable_persona(Some(&manifest), "reviewer").is_err());
    assert_eq!(
        resolve_discoverable_persona(Some(&manifest), "zeta/reviewer")
            .unwrap()
            .id,
        "zeta/reviewer"
    );
    assert_eq!(
        resolve_discoverable_persona(Some(&manifest), "alpha/reviewer")
            .unwrap()
            .id,
        "alpha/reviewer"
    );

    fs::remove_file(current_packages_dir(tmp.path()).join("zeta/workflow.harn")).unwrap();
    assert_eq!(
        resolve_discoverable_persona(Some(&manifest), "alpha/reviewer")
            .unwrap()
            .id,
        "alpha/reviewer"
    );
    assert!(load_discoverable_personas(Some(&manifest))
        .unwrap_err()
        .contains("zeta"));
}

#[test]
fn persona_catalog_rejects_locked_exports_without_a_published_generation() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join(MANIFEST),
        "[package]\nname = \"consumer\"\n",
    )
    .unwrap();
    let lock = LockFile {
        packages: vec![LockEntry {
            name: "agents".to_string(),
            source: "path+../agents".to_string(),
            exports: PackageLockExports {
                personas: vec!["reviewer".to_string()],
                ..PackageLockExports::default()
            },
            ..LockEntry::default()
        }],
        ..LockFile::default()
    };
    fs::write(
        tmp.path().join(LOCK_FILE),
        toml::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    let error = load_discoverable_personas(Some(&tmp.path().join(MANIFEST))).unwrap_err();
    assert!(error.contains("package-current.toml"), "{error}");
    assert!(error.contains("harn install"), "{error}");
}

#[test]
fn package_doctor_rejects_stale_locked_persona_names() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join(MANIFEST),
        "[package]\nname = \"consumer\"\n",
    )
    .unwrap();
    install_test_persona_package(
        tmp.path(),
        "agents",
        vec!["removed".to_string()],
        &["reviewer"],
    );

    let workspace = TestWorkspace::new(tmp.path());
    let report = doctor_packages_in(workspace.env()).unwrap();

    assert!(!report.ok);
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "persona-exports-stale"
            && diagnostic.message.contains("removed")
            && diagnostic.message.contains("reviewer")
    }));
    assert!(report.personas.is_empty());
}

#[test]
fn package_doctor_rejects_missing_persona_manifest_and_entry_workflow() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join(MANIFEST),
        "[package]\nname = \"consumer\"\n",
    )
    .unwrap();
    install_test_persona_package(
        tmp.path(),
        "agents",
        vec!["reviewer".to_string()],
        &["reviewer"],
    );
    let package_dir = current_packages_dir(tmp.path()).join("agents");
    let workspace = TestWorkspace::new(tmp.path());

    fs::remove_file(package_dir.join("workflow.harn")).unwrap();
    refresh_test_package_hash(tmp.path(), &package_dir);
    let report = doctor_packages_in(workspace.env()).unwrap();
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "persona-entry-workflow-invalid"
            && diagnostic.message.contains("workflow.harn")
    }));

    fs::write(
        package_dir.join("workflow.harn"),
        "pipeline run(task) -> dict { return {ok: true} }\n",
    )
    .unwrap();
    refresh_test_package_hash(tmp.path(), &package_dir);
    let report = doctor_packages_in(workspace.env()).unwrap();
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "persona-entry-workflow-invalid"
            && diagnostic.message.contains("is not exported")
    }));

    fs::write(
        package_dir.join(MANIFEST),
        r#"[package]
name = "agents"
version = "1.2.3"

[[personas]]
name = "reviewer"
description = "Invalid authority."
entry_workflow = "workflow.harn#run"
tools = ["filesystem"]
autonomy_tier = "administrator"
receipt_policy = "required"
"#,
    )
    .unwrap();
    refresh_test_package_hash(tmp.path(), &package_dir);
    let report = doctor_packages_in(workspace.env()).unwrap();
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "persona-manifest-invalid"
            && diagnostic.message.contains("autonomy_tier")
    }));

    fs::remove_file(package_dir.join(MANIFEST)).unwrap();
    refresh_test_package_hash(tmp.path(), &package_dir);
    let report = doctor_packages_in(workspace.env()).unwrap();
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "persona-package-integrity-failed"
            && diagnostic.message.contains(MANIFEST)
    }));
}

fn refresh_test_package_hash(project_root: &Path, package_dir: &Path) {
    let mut lock = LockFile::load(&project_root.join(LOCK_FILE))
        .unwrap()
        .unwrap();
    lock.packages[0].content_hash = Some(compute_content_hash(package_dir).unwrap());
    let body = toml::to_string_pretty(&lock).unwrap();
    fs::write(project_root.join(LOCK_FILE), &body).unwrap();
    write_test_generation_lock(project_root, &body);
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
