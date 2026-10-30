//! End-to-end tests for the scanner host capability.
//!
//! Builds a small fixture repository inside a temp dir and asserts every
//! piece of the `ScanResult` shape: file discovery, file roles
//! (`.gitignore` + excluded-dir filtering), symbol/import extraction,
//! dependency edges, test pairings, folder + project metadata,
//! sub-project boundaries, repo-map text, and the
//! `scan_project → scan_incremental` round-trip.
//!
//! The fixture below is hand-shaped to exercise common sub-tree patterns
//! (Cargo + nested package.json, `__tests__/` test files,
//! paginated-response helpers, prisma schema) without forcing a
//! heavyweight checkout.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use harn_hostlib::scanner::{
    discover_test_commands, scan_incremental, scan_project, scan_project_with_git, FileRecord,
    GitCapabilities, ProjectMetadata, ScanProjectOptions, ScanResult, SymbolKind,
    TestCommandDiscovery, TestLayoutDiscovery, TestLayoutProfile,
};
use harn_vm::ignore_policy::IgnorePolicy;
use tempfile::tempdir;

#[cfg(unix)]
#[test]
fn package_script_aliases_stay_within_physical_project() {
    let fixture = tempdir().unwrap();
    let project = fixture.path().join("project");
    let outside = fixture.path().join("outside");
    write(
        &project,
        "package.json",
        r#"{"name":"inside","packageManager":"pnpm@10.0.0","scripts":{"test":"echo KNOWN_INSIDE"}}"#,
    );
    write(
        &project,
        "owned/package.json",
        r#"{"name":"owned","packageManager":"npm@10.0.0","scripts":{"test":"echo KNOWN_OWNED"}}"#,
    );
    write(
        &outside,
        "package.json",
        r#"{"name":"outside","packageManager":"npm@10.0.0","scripts":{"test":"echo KNOWN_OUTSIDE"}}"#,
    );
    fs::create_dir(project.join("packages")).unwrap();
    std::os::unix::fs::symlink(&outside, project.join("packages/escaped")).unwrap();
    std::os::unix::fs::symlink(project.join("owned"), project.join("packages/aliased")).unwrap();
    write(
        &project,
        "packages/escaped-manifest/index.js",
        "// known source",
    );
    std::os::unix::fs::symlink(
        outside.join("package.json"),
        project.join("packages/escaped-manifest/package.json"),
    )
    .unwrap();
    write(
        &project,
        "packages/linked-manifest/index.js",
        "// known source",
    );
    std::os::unix::fs::symlink(
        project.join("owned/package.json"),
        project.join("packages/linked-manifest/package.json"),
    )
    .unwrap();
    write(
        &project,
        "physical-package/package.json",
        r#"{"scripts":{"test":"echo KNOWN_PHYSICAL_LOCK"}}"#,
    );
    write(&project, "physical-package/yarn.lock", "");
    write(&project, "packages/linked-lock/index.js", "// known source");
    std::os::unix::fs::symlink(
        project.join("physical-package/package.json"),
        project.join("packages/linked-lock/package.json"),
    )
    .unwrap();
    write(
        &project,
        "packages/inherited-lock/package.json",
        r#"{"scripts":{"test":"echo KNOWN_INHERITED_LOCK"}}"#,
    );
    std::os::unix::fs::symlink(
        project.join("physical-package/package.json"),
        project.join("packages/package.json"),
    )
    .unwrap();
    let initial_alias = fixture.path().join("initial-alias");
    std::os::unix::fs::symlink(&project, &initial_alias).unwrap();

    for root in [&project, &initial_alias] {
        let result = scan_project(
            root,
            ScanProjectOptions {
                include_git_history: false,
                ..ScanProjectOptions::default()
            },
        );
        let commands = test_commands(&result.project);
        assert!(commands.get("pnpm test").unwrap().contains("KNOWN_INSIDE"));
        assert!(commands
            .get("cd packages/aliased && npm test")
            .unwrap()
            .contains("KNOWN_OWNED"));
        assert!(!commands.contains_key("cd packages/escaped && npm test"));
        assert!(commands
            .get("cd packages/linked-manifest && npm test")
            .unwrap()
            .contains("KNOWN_OWNED"));
        assert!(commands
            .get("cd packages/linked-lock && yarn run test")
            .unwrap()
            .contains("KNOWN_PHYSICAL_LOCK"));
        assert!(commands
            .get("cd packages/inherited-lock && yarn run test")
            .unwrap()
            .contains("KNOWN_INHERITED_LOCK"));
        assert!(!commands
            .values()
            .any(|label| label.contains("KNOWN_OUTSIDE")));
    }
    write(
        &project,
        "packages/inherited/package.json",
        r#"{"scripts":{"test":"echo KNOWN_INHERITED"}}"#,
    );
    fs::remove_file(project.join("packages/package.json")).unwrap();
    fs::remove_file(project.join("package.json")).unwrap();
    std::os::unix::fs::symlink(outside.join("package.json"), project.join("package.json")).unwrap();
    let result = scan_project(
        &project,
        ScanProjectOptions {
            include_git_history: false,
            ..ScanProjectOptions::default()
        },
    );
    assert!(!test_commands(&result.project)
        .values()
        .any(|body| body.contains("KNOWN_OUTSIDE") || body.contains("KNOWN_INHERITED")));
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
}

fn test_commands(project: &ProjectMetadata) -> &BTreeMap<String, String> {
    match &project.test_command_discovery {
        TestCommandDiscovery::Complete { commands, .. } => commands,
        TestCommandDiscovery::Unavailable { detail } => {
            panic!("test discovery unavailable: {detail}")
        }
    }
}

fn scan_without_git_history(root: &Path) -> ScanResult {
    scan_project(
        root,
        ScanProjectOptions {
            include_git_history: false,
            ..ScanProjectOptions::default()
        },
    )
}

#[test]
fn catalog_layout_discovery_distinguishes_source_tests_and_ambiguity() {
    let tmp = tempdir().unwrap();
    let profiles = vec![
        TestLayoutProfile {
            language: "shell".into(),
            extensions: vec!["sh".into(), "bats".into()],
            patterns: vec![".bats".into(), "_test.sh".into()],
        },
        TestLayoutProfile {
            language: "python".into(),
            extensions: vec!["py".into()],
            patterns: vec!["test_".into(), "_test.py".into()],
        },
    ];
    write(tmp.path(), "app.sh", "echo source-only\n");
    write(tmp.path(), "test_helper.py", "def helper(): pass\n");
    let unconfigured = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &[]);
    assert!(matches!(
        unconfigured,
        TestCommandDiscovery::Complete {
            layout: TestLayoutDiscovery::Unconfigured,
            ..
        }
    ));
    let empty = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &profiles);
    assert!(matches!(
        empty,
        TestCommandDiscovery::Complete {
            layout: TestLayoutDiscovery::Empty,
            ..
        }
    ));

    write(tmp.path(), "example.bats", "@test works { true; }\n");
    write(tmp.path(), "tests/nested/second_test.sh", "true\n");
    let discovered = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &profiles);
    assert!(matches!(&discovered, TestCommandDiscovery::Complete {
        layout: TestLayoutDiscovery::Single { language, files }, ..
    } if language == "shell" && files.iter().map(String::as_str).collect::<Vec<_>>() == vec!["example.bats", "tests/nested/second_test.sh"]));
    let opts = ScanProjectOptions {
        include_git_history: false,
        layout_profiles: profiles.clone(),
        ..ScanProjectOptions::default()
    };
    let full = scan_project(tmp.path(), opts.clone());
    assert_eq!(discovered, full.project.test_command_discovery);
    let incremental = scan_incremental(&full.snapshot_token, None, opts);
    assert_eq!(
        discovered,
        incremental.result.project.test_command_discovery
    );

    write(
        tmp.path(),
        "tests/test_value.py",
        "def test_value(): pass\n",
    );
    let ambiguous = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &profiles);
    assert!(
        matches!(ambiguous, TestCommandDiscovery::Unavailable { detail } if detail.contains("Multiple test layouts"))
    );
    write(tmp.path(), "Makefile", "test:\n\t@echo passed\n");
    let declared = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &profiles);
    assert!(matches!(declared, TestCommandDiscovery::Complete {
        commands, layout: TestLayoutDiscovery::Ambiguous { languages }, ..
    } if commands.len() == 1 && commands.contains_key("make test") && languages.iter().map(String::as_str).collect::<Vec<_>>() == vec!["python", "shell"]));
    fs::remove_file(tmp.path().join("Makefile")).unwrap();
    write(
        tmp.path(),
        "apps/child/package.json",
        r#"{"scripts":{"test":"echo child-only"}}"#,
    );
    let child_only = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &profiles);
    assert!(matches!(
        child_only,
        TestCommandDiscovery::Unavailable { .. }
    ));
    write(
        tmp.path(),
        "package.json",
        r#"{"scripts":{"lint":"echo lint"}}"#,
    );
    let lint_only = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &profiles);
    assert!(matches!(
        lint_only,
        TestCommandDiscovery::Unavailable { .. }
    ));
    write(
        tmp.path(),
        "package.json",
        r#"{"scripts":{"test:unit":"echo unit"}}"#,
    );
    let partial_only = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &profiles);
    assert!(matches!(
        partial_only,
        TestCommandDiscovery::Unavailable { .. }
    ));
    write(
        tmp.path(),
        "package.json",
        r#"{"scripts":{"lint":"echo lint","test":"echo test"}}"#,
    );
    let package_test = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &profiles);
    assert!(
        matches!(package_test, TestCommandDiscovery::Complete { commands, .. }
        if commands.len() == 1 && commands.contains_key("npm test"))
    );
}

#[test]
fn discovery_only_matches_full_scan_and_rechecks_without_a_snapshot() {
    let tmp = tempdir().unwrap();
    write(
        tmp.path(),
        "test_app.py",
        "def test_value():\n    assert True\n",
    );
    let discovery = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &[]);
    assert!(
        matches!(&discovery, TestCommandDiscovery::Complete { commands, .. } if commands.contains_key("python -m pytest"))
    );
    assert!(!tmp
        .path()
        .join(".harn/hostlib/scanner-snapshot.json")
        .exists());
    let full = scan_without_git_history(tmp.path());
    assert_eq!(discovery, full.project.test_command_discovery);
    fs::remove_file(tmp.path().join("test_app.py")).unwrap();
    assert!(matches!(
        discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &[]),
        TestCommandDiscovery::Complete { commands, .. } if commands.is_empty()
    ));
}

#[test]
fn discovery_reports_an_unreadable_git_index_instead_of_a_false_empty_search() {
    let tmp = tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap()
        .success());
    let empty = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &[]);
    assert!(
        matches!(empty, TestCommandDiscovery::Complete { commands, .. } if commands.is_empty())
    );
    write(tmp.path(), ".gitignore", "test_value.py\n");
    write(tmp.path(), "test_value.py", "def test_value(): pass\n");
    assert!(std::process::Command::new("git")
        .args(["add", "-f", "test_value.py"])
        .current_dir(tmp.path())
        .status()
        .unwrap()
        .success());
    let before = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &[]);
    assert!(
        matches!(before, TestCommandDiscovery::Complete { commands, .. }
        if commands.contains_key("python -m pytest"))
    );
    let opts = ScanProjectOptions {
        include_git_history: false,
        ..ScanProjectOptions::default()
    };
    let full_before = scan_project(tmp.path(), opts.clone());
    assert!(matches!(
        full_before.project.test_command_discovery,
        TestCommandDiscovery::Complete { .. }
    ));
    let index = fs::read(tmp.path().join(".git/index")).unwrap();
    fs::write(tmp.path().join(".git/index"), b"corrupt index").unwrap();
    let failed = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &[]);
    assert!(
        matches!(failed, TestCommandDiscovery::Unavailable { .. }),
        "{failed:?}"
    );
    let incremental = scan_incremental(&full_before.snapshot_token, None, opts.clone());
    assert!(matches!(
        incremental.result.project.test_command_discovery,
        TestCommandDiscovery::Unavailable { .. }
    ));
    let full_failed = scan_project(tmp.path(), opts.clone());
    assert!(matches!(
        full_failed.project.test_command_discovery,
        TestCommandDiscovery::Unavailable { .. }
    ));
    fs::write(tmp.path().join(".git/index"), index).unwrap();
    let recovered = scan_incremental(&full_failed.snapshot_token, None, opts);
    assert!(
        matches!(recovered.result.project.test_command_discovery, TestCommandDiscovery::Complete { commands, .. } if commands.contains_key("python -m pytest"))
    );
}

#[test]
fn discovery_only_finds_a_git_tracked_test_matched_by_ignore() {
    let tmp = tempdir().unwrap();
    write(tmp.path(), ".gitignore", "test_app.py\n");
    write(
        tmp.path(),
        "test_app.py",
        "def test_value():\n    assert True\n",
    );
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .args(["add", "-f", "test_app.py"])
        .current_dir(tmp.path())
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .args(["check-ignore", "--no-index", "-q", "test_app.py"])
        .current_dir(tmp.path())
        .status()
        .unwrap()
        .success());
    let discovery = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &[]);
    assert!(matches!(
        discovery,
        TestCommandDiscovery::Complete { commands, .. } if commands.contains_key("python -m pytest")
    ));
}

#[test]
fn tracked_ignored_unindexed_test_has_the_same_layout_in_all_scans() {
    let tmp = tempdir().unwrap();
    write(tmp.path(), ".gitignore", "*.bats\n");
    write(tmp.path(), "tracked.bats", "@test works { true; }\n");
    write(tmp.path(), "ignored.bats", "@test ignored { true; }\n");
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(tmp.path())
        .status()
        .unwrap()
        .success());
    assert!(std::process::Command::new("git")
        .args(["add", "-f", "tracked.bats"])
        .current_dir(tmp.path())
        .status()
        .unwrap()
        .success());
    let profiles = vec![TestLayoutProfile {
        language: "shell".into(),
        extensions: vec!["bats".into()],
        patterns: vec![".bats".into()],
    }];
    let fresh = discover_test_commands(tmp.path(), false, IgnorePolicy::Project, &profiles);
    assert!(matches!(&fresh, TestCommandDiscovery::Complete {
        layout: TestLayoutDiscovery::Single { language, files }, ..
    } if language == "shell" && files.iter().map(String::as_str).collect::<Vec<_>>() == vec!["tracked.bats"]));
    let opts = ScanProjectOptions {
        include_git_history: false,
        layout_profiles: profiles,
        ..ScanProjectOptions::default()
    };
    let full = scan_project(tmp.path(), opts.clone());
    assert_eq!(fresh, full.project.test_command_discovery);
    let incremental = scan_incremental(&full.snapshot_token, None, opts.clone());
    assert_eq!(fresh, incremental.result.project.test_command_discovery);
    assert!(std::process::Command::new("git")
        .args(["rm", "--cached", "-q", "tracked.bats"])
        .current_dir(tmp.path())
        .status()
        .unwrap()
        .success());
    let removed = scan_incremental(&full.snapshot_token, None, opts.clone());
    assert!(matches!(
        removed.result.project.test_command_discovery,
        TestCommandDiscovery::Complete {
            layout: TestLayoutDiscovery::Empty,
            ..
        }
    ));
    assert!(std::process::Command::new("git")
        .args(["add", "-f", "tracked.bats"])
        .current_dir(tmp.path())
        .status()
        .unwrap()
        .success());
    let restored = scan_incremental(&full.snapshot_token, None, opts);
    assert_eq!(fresh, restored.result.project.test_command_discovery);
}

#[test]
fn bare_root_pytest_with_import_has_a_discovered_test_command() {
    let tmp = tempdir().unwrap();
    write(
        tmp.path(),
        "test_app.py",
        "import pytest\n\ndef test_value():\n    assert 2 + 2 == 4\n",
    );
    let result = scan_without_git_history(tmp.path());
    assert!(result
        .files
        .iter()
        .any(|file| file.relative_path == "test_app.py"));
    assert!(test_commands(&result.project).contains_key("python -m pytest"));
}

#[test]
fn bare_root_pytest_without_import_has_a_discovered_test_command() {
    let tmp = tempdir().unwrap();
    write(
        tmp.path(),
        "test_app.py",
        "def test_value():\n    assert 2 + 2 == 4\n",
    );
    let result = scan_without_git_history(tmp.path());
    assert!(result
        .files
        .iter()
        .any(|file| file.relative_path == "test_app.py"));
    assert!(result
        .symbols
        .iter()
        .any(|symbol| symbol.name == "test_value"));
    assert!(test_commands(&result.project).contains_key("python -m pytest"));
}

#[test]
fn bare_source_without_tests_has_no_discovered_test_command() {
    let tmp = tempdir().unwrap();
    write(tmp.path(), "app.py", "def value():\n    return 4\n");
    write(
        tmp.path(),
        "tests/helpers.py",
        "def test_helper():\n    pass\n",
    );
    write(tmp.path(), "test_no_tests.py", "def helper():\n    pass\n");
    let result = scan_without_git_history(tmp.path());
    assert!(result
        .files
        .iter()
        .any(|file| file.relative_path == "app.py"));
    assert!(result
        .symbols
        .iter()
        .any(|symbol| symbol.name == "test_helper"));
    assert!(test_commands(&result.project).is_empty());
}

#[cfg(not(windows))]
#[test]
fn non_collected_uppercase_python_filename_has_no_test_command() {
    let tmp = tempdir().unwrap();
    write(
        tmp.path(),
        "TEST_app.py",
        "def test_value():\n    assert True\n",
    );
    let result = scan_without_git_history(tmp.path());
    assert!(result
        .symbols
        .iter()
        .any(|symbol| symbol.name == "test_value"));
    assert!(test_commands(&result.project).is_empty());
}

#[cfg(windows)]
#[test]
fn windows_pytest_filename_matching_is_case_insensitive() {
    let tmp = tempdir().unwrap();
    write(
        tmp.path(),
        "TEST_app.py",
        "def test_value():\n    assert True\n",
    );
    let result = scan_without_git_history(tmp.path());
    assert!(test_commands(&result.project).contains_key("python -m pytest"));
}

#[test]
fn nested_pytest_with_import_remains_a_discovered_test_command() {
    let tmp = tempdir().unwrap();
    write(
        tmp.path(),
        "tests/test_app.py",
        "import pytest\n\ndef test_value():\n    assert 2 + 2 == 4\n",
    );
    let result = scan_without_git_history(tmp.path());
    assert!(result
        .files
        .iter()
        .any(|file| file.relative_path == "tests/test_app.py"));
    assert!(test_commands(&result.project).contains_key("python -m pytest"));
}

#[test]
fn nested_pytest_without_import_has_a_discovered_test_command() {
    let tmp = tempdir().unwrap();
    write(
        tmp.path(),
        "tests/test_app.py",
        "def test_value():\n    assert 2 + 2 == 4\n",
    );
    let result = scan_without_git_history(tmp.path());
    assert!(result
        .files
        .iter()
        .any(|file| file.relative_path == "tests/test_app.py"));
    assert!(test_commands(&result.project).contains_key("python -m pytest"));
}

#[test]
fn a_test_added_after_scanning_updates_discovered_commands() {
    let tmp = tempdir().unwrap();
    write(tmp.path(), "app.py", "def value():\n    return 4\n");
    let options = ScanProjectOptions {
        include_git_history: false,
        ..ScanProjectOptions::default()
    };
    let initial = scan_project(tmp.path(), options.clone());
    assert!(test_commands(&initial.project).is_empty());
    write(
        tmp.path(),
        "test_app.py",
        "def test_value():\n    assert 2 + 2 == 4\n",
    );
    let changed = vec!["test_app.py".to_string()];
    let refreshed = scan_incremental(&initial.snapshot_token, Some(&changed), options);
    assert!(refreshed
        .result
        .files
        .iter()
        .any(|file| file.relative_path == "test_app.py"));
    assert!(test_commands(&refreshed.result.project).contains_key("python -m pytest"));
}

#[cfg(unix)]
#[test]
fn unreadable_test_directory_is_unavailable_and_recovers() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().unwrap();
    write(
        tmp.path(),
        "locked/test_app.py",
        "def test_value():\n    assert 2 + 2 == 4\n",
    );
    let before = scan_without_git_history(tmp.path());
    assert!(test_commands(&before.project).contains_key("python -m pytest"));
    let directory = tmp.path().join("locked");
    let original = fs::metadata(&directory).unwrap().permissions();
    let mut denied = original.clone();
    denied.set_mode(0o0);
    fs::set_permissions(&directory, denied).unwrap();
    let read_error = fs::read_dir(&directory).err();
    let inaccessible = scan_without_git_history(tmp.path());
    fs::set_permissions(&directory, original).unwrap();
    assert_eq!(
        read_error.map(|error| error.kind()),
        Some(std::io::ErrorKind::PermissionDenied),
        "the operating system must actually deny the directory read"
    );
    assert!(matches!(
        &inaccessible.project.test_command_discovery,
        TestCommandDiscovery::Unavailable { detail } if detail.contains("locked")
    ));
    let recovered = scan_without_git_history(tmp.path());
    assert!(test_commands(&recovered.project).contains_key("python -m pytest"));
}

#[cfg(unix)]
#[test]
fn unchanged_files_do_not_hide_discovery_denial_or_recovery() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().unwrap();
    write(tmp.path(), "app.py", "def value():\n    return 4\n");
    let directory = tmp.path().join("locked");
    fs::create_dir(&directory).unwrap();
    let options = ScanProjectOptions {
        include_git_history: false,
        ..ScanProjectOptions::default()
    };
    let initial = scan_project(tmp.path(), options.clone());
    assert!(test_commands(&initial.project).is_empty());
    let original = fs::metadata(&directory).unwrap().permissions();
    let mut denied = original.clone();
    denied.set_mode(0o0);
    fs::set_permissions(&directory, denied).unwrap();
    let read_error = fs::read_dir(&directory).err();
    let inaccessible = scan_incremental(&initial.snapshot_token, None, options.clone());
    fs::set_permissions(&directory, original).unwrap();
    assert_eq!(
        read_error.map(|error| error.kind()),
        Some(std::io::ErrorKind::PermissionDenied)
    );
    assert!(inaccessible.delta.added.is_empty());
    assert!(inaccessible.delta.modified.is_empty());
    assert!(inaccessible.delta.removed.is_empty());
    assert!(matches!(
        inaccessible.result.project.test_command_discovery,
        TestCommandDiscovery::Unavailable { .. }
    ));
    let recovered = scan_incremental(&initial.snapshot_token, None, options);
    assert!(test_commands(&recovered.result.project).is_empty());
}

#[cfg(unix)]
#[test]
fn unreadable_test_file_cannot_expose_a_partial_command_set() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().unwrap();
    write(tmp.path(), "test_a.py", "def test_a():\n    assert True\n");
    write(tmp.path(), "test_b.py", "def test_b():\n    assert True\n");
    let file = tmp.path().join("test_b.py");
    let original = fs::metadata(&file).unwrap().permissions();
    let mut denied = original.clone();
    denied.set_mode(0o0);
    fs::set_permissions(&file, denied).unwrap();
    let read_error = fs::read_to_string(&file).err();
    let result = scan_without_git_history(tmp.path());
    fs::set_permissions(&file, original).unwrap();
    assert_eq!(
        read_error.map(|error| error.kind()),
        Some(std::io::ErrorKind::PermissionDenied)
    );
    assert!(matches!(
        result.project.test_command_discovery,
        TestCommandDiscovery::Unavailable { .. }
    ));
}

#[test]
fn invalid_package_manifest_is_not_a_complete_empty_discovery() {
    let tmp = tempdir().unwrap();
    write(tmp.path(), "package.json", "{not valid json}");
    let result = scan_without_git_history(tmp.path());
    assert!(matches!(
        result.project.test_command_discovery,
        TestCommandDiscovery::Unavailable { .. }
    ));
}

#[test]
fn malformed_ignore_policy_cannot_claim_a_complete_discovery() {
    let tmp = tempdir().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    write(tmp.path(), ".gitignore", "[z-a]\n");
    write(tmp.path(), "app.py", "def value():\n    return 4\n");
    let mut invalid = ignore::gitignore::GitignoreBuilder::new(tmp.path());
    assert!(invalid.add(tmp.path().join(".gitignore")).is_some());
    let result = scan_without_git_history(tmp.path());
    assert!(
        matches!(
            &result.project.test_command_discovery,
            TestCommandDiscovery::Unavailable { detail } if detail.contains(".gitignore")
        ),
        "actual discovery: {:?}",
        result.project.test_command_discovery
    );
}

#[test]
fn discovery_status_is_required_in_the_serialized_project() {
    let tmp = tempdir().unwrap();
    let result = scan_without_git_history(tmp.path());
    assert!(test_commands(&result.project).is_empty());
    let mut project = serde_json::to_value(&result.project).unwrap();
    assert_eq!(project["test_command_discovery"]["state"], "complete");
    assert!(project.get("test_commands").is_none());
    assert!(project.get("detected_test_command").is_none());
    let mut missing_preferred = project.clone();
    missing_preferred["test_command_discovery"]
        .as_object_mut()
        .unwrap()
        .remove("preferred");
    assert!(serde_json::from_value::<ProjectMetadata>(missing_preferred).is_err());
    let mut empty_detail = project.clone();
    empty_detail["test_command_discovery"] =
        serde_json::json!({"state": "unavailable", "detail": "  "});
    assert!(serde_json::from_value::<ProjectMetadata>(empty_detail).is_err());
    project
        .as_object_mut()
        .unwrap()
        .remove("test_command_discovery");
    assert!(serde_json::from_value::<ProjectMetadata>(project).is_err());
}

fn build_fixture(root: &Path) {
    write(
        root,
        "Cargo.toml",
        r#"[package]
name = "scanner-fixture"
version = "0.1.0"
edition = "2024"
"#,
    );
    write(
        root,
        "package.json",
        r#"{
  "name": "scanner-fixture",
  "scripts": {
    "test": "vitest run",
    "lint": "eslint .",
    "build": "tsc -b"
  },
  "devDependencies": { "vitest": "latest" }
}
"#,
    );
    write(
        root,
        "AGENTS.md",
        "Use scanner-owned context projections.\n",
    );
    write(
        root,
        "src/main.rs",
        r"use crate::routes::accounts;

pub struct App;

impl App {
    pub fn run(&self) {}
}

pub fn entry() {}
",
    );
    write(
        root,
        "src/routes/accounts.rs",
        r"use std::collections::HashMap;

pub struct AccountsService;

impl AccountsService {
    pub fn lookup(&self, id: u64) -> Option<()> { None }
}
",
    );
    write(
        root,
        "src/routes/accounts_test.rs",
        r"use super::accounts::AccountsService;
",
    );
    write(
        root,
        "src/routes/errors.ts",
        "export function reject(res: any, message: string) { res.status(400).json({ error: message }); }\n",
    );
    write(
        root,
        "src/lib/helpers.ts",
        r"export function paginatedResponse<T>(items: T[], total: number) {
  return { items, total, page: 1 };
}

export function asyncHandler(fn: any) { return fn; }
",
    );
    write(
        root,
        "src/lib/__tests__/helpers.test.ts",
        r#"import { expect } from "vitest";
import { paginatedResponse } from "../helpers";

beforeEach(() => createTestUser());
expect(paginatedResponse([], 0).total).toBe(0);

export function createTestUser(name: string = "Ada") { return { name }; }
"#,
    );
    write(root, "prisma/schema.prisma", "model User { id Int @id }\n");
    // Hidden + excluded dirs that must never appear in the result.
    write(root, ".env", "SECRET=1\n");
    write(root, "node_modules/foo/index.js", "module.exports = 1;\n");
    write(root, "target/debug/junk.txt", "ignore me\n");
    // Nested sub-project at a 2nd-level path.
    write(
        root,
        "services/api/Cargo.toml",
        r#"[package]
name = "api"
version = "0.1.0"
edition = "2024"
"#,
    );
    write(
        root,
        "services/api/src/lib.rs",
        "pub fn ping() -> bool { true }\n",
    );
}

fn touch_after_snapshot(path: &Path, snapshot_max_mtime_ms: i64) {
    let base_secs = snapshot_max_mtime_ms.max(0).div_euclid(1_000);
    let new_mtime = filetime::FileTime::from_unix_time(base_secs.saturating_add(60), 0);
    filetime::set_file_mtime(path, new_mtime).unwrap();
}

#[derive(Default)]
struct MockGit {
    files: Option<Vec<String>>,
    churn: std::collections::BTreeMap<String, f64>,
}

impl GitCapabilities for MockGit {
    fn list_files(&self, _root: &Path) -> Result<Option<Vec<String>>, String> {
        Ok(self.files.clone())
    }

    fn churn_scores(&self, _root: &Path) -> std::collections::BTreeMap<String, f64> {
        self.churn.clone()
    }
}

#[test]
fn git_tracked_ignored_test_still_contributes_a_command() {
    let tmp = tempdir().unwrap();
    fs::create_dir(tmp.path().join(".git")).unwrap();
    write(tmp.path(), ".gitignore", "test_app.py\n");
    write(
        tmp.path(),
        "test_app.py",
        "def test_value():\n    assert True\n",
    );
    let mut walker = ignore::WalkBuilder::new(tmp.path());
    harn_vm::ignore_policy::configure(
        &mut walker,
        tmp.path(),
        harn_vm::ignore_policy::IgnorePolicy::Project,
        false,
    )
    .unwrap();
    assert!(!walker
        .build()
        .any(|entry| { entry.unwrap().path() == tmp.path().join("test_app.py") }));
    let git = MockGit {
        files: Some(vec!["test_app.py".into()]),
        ..MockGit::default()
    };
    let result = scan_project_with_git(tmp.path(), ScanProjectOptions::default(), &git);
    assert!(result
        .files
        .iter()
        .any(|file| file.relative_path == "test_app.py"));
    assert!(test_commands(&result.project).contains_key("python -m pytest"));
}

#[test]
fn scan_project_emits_full_result_shape() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let result = scan_project(tmp.path(), ScanProjectOptions::default());

    // Discovery: source files present, excluded dirs gone.
    let paths: Vec<&str> = result
        .files
        .iter()
        .map(|f| f.relative_path.as_str())
        .collect();
    assert!(paths.contains(&"src/main.rs"));
    assert!(paths.contains(&"src/routes/accounts.rs"));
    assert!(paths.contains(&"src/lib/helpers.ts"));
    assert!(!paths.iter().any(|p| p.starts_with("node_modules")));
    assert!(!paths.iter().any(|p| p.starts_with("target")));

    // File records have language + line counts.
    let main = result
        .files
        .iter()
        .find(|f| f.relative_path == "src/main.rs")
        .expect("main.rs file record");
    assert_eq!(main.language, "rust");
    assert!(main.line_count > 0);
    assert!(main.size_bytes > 0);
    assert!(main.imports.iter().any(|i| i.contains("routes")));

    // Symbol records cover both Rust and TS source.
    let symbol_names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(symbol_names.contains(&"App"));
    assert!(symbol_names.contains(&"AccountsService"));
    assert!(symbol_names.contains(&"paginatedResponse"));

    // Dependency edges built from imports.
    assert!(result
        .dependencies
        .iter()
        .any(|d| d.from_file == "src/routes/accounts.rs" && d.to_module.contains("HashMap")));

    // Test-source pairing: helpers.ts ↔ helpers.test.ts.
    let helpers = result
        .files
        .iter()
        .find(|f| f.relative_path == "src/lib/helpers.ts")
        .unwrap();
    assert_eq!(
        helpers.corresponding_test_file.as_deref(),
        Some("src/lib/__tests__/helpers.test.ts")
    );

    // Folder records sorted by line count desc.
    assert!(!result.folders.is_empty());
    for window in result.folders.windows(2) {
        assert!(window[0].line_count >= window[1].line_count);
    }

    // Project metadata: language stats, test commands, code patterns.
    assert_eq!(result.project.name, project_name(tmp.path()));
    assert!(result.project.languages.iter().any(|l| l.name == "rust"));
    assert!(result
        .project
        .languages
        .iter()
        .any(|l| l.name == "typescript"));
    assert!(test_commands(&result.project).contains_key("cargo test"));
    assert!(test_commands(&result.project).contains_key("npm test"));
    let detected = match &result.project.test_command_discovery {
        TestCommandDiscovery::Complete {
            preferred: Some(command),
            ..
        } => command.as_str(),
        _ => panic!("a test command should be detected"),
    };
    assert!(!detected.is_empty());
    assert!(result
        .project
        .code_patterns
        .iter()
        .any(|p| p.contains("Prisma")));

    // Sub-project detection: outer fixture + nested api crate.
    let sub_marker_count = result
        .sub_projects
        .iter()
        .filter(|s| s.project_marker == "Cargo.toml" || s.project_marker == "package.json")
        .count();
    assert!(sub_marker_count >= 2);

    // Repo map renders symbols.
    assert!(result.repo_map.contains("src/main.rs"));
    assert!(result.repo_map.contains("App"));

    // Scanner-owned context projection: the canonical scan path reached the
    // fingerprint implementation and exposed its result on the public shape.
    let fingerprint = &result.codebase_fingerprint;
    for expected in [
        "# Codebase fingerprint",
        "## Naming conventions",
        "## Error handling",
        "## Test recipe",
        "## Test commands",
        "## Code patterns",
        "## Test helpers",
        "## Dependencies\n- `vitest`",
        "## Agent instructions\nUse scanner-owned context projections.",
    ] {
        assert!(
            fingerprint.contains(expected),
            "missing {expected:?} in:\n{fingerprint}"
        );
    }

    // Output is deterministic — files sorted alphabetically.
    let sorted: Vec<_> = result
        .files
        .iter()
        .map(|f| &f.relative_path)
        .collect::<Vec<_>>();
    let mut copy = sorted.clone();
    copy.sort();
    assert_eq!(sorted, copy);

    // Snapshot persisted at the canonical location.
    let snapshot_path = tmp.path().join(".harn/hostlib/scanner-snapshot.json");
    assert!(snapshot_path.exists());
}

#[test]
fn scan_project_uses_injected_git_capabilities() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let git = MockGit {
        files: Some(vec![
            "Cargo.toml".to_string(),
            "src/main.rs".to_string(),
            "src/routes/accounts.rs".to_string(),
            "target/debug/junk.txt".to_string(),
        ]),
        churn: std::iter::once(("src/main.rs".to_string(), 0.75)).collect(),
    };
    let result = scan_project_with_git(tmp.path(), ScanProjectOptions::default(), &git);
    let paths: Vec<&str> = result
        .files
        .iter()
        .map(|f| f.relative_path.as_str())
        .collect();

    assert_eq!(
        paths,
        vec!["Cargo.toml", "src/main.rs", "src/routes/accounts.rs"]
    );
    let main = find_file(&result.files, "src/main.rs").unwrap();
    assert_eq!(main.churn_score, 0.75);
}

fn project_name(root: &Path) -> String {
    let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    canonical
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

#[test]
fn scan_incremental_picks_up_added_and_removed_files() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let initial = scan_project(tmp.path(), ScanProjectOptions::default());
    let token = initial.snapshot_token.clone();
    let snapshot_max_mtime_ms = initial
        .files
        .iter()
        .map(|file| file.last_modified_unix_ms)
        .max()
        .unwrap_or(0);

    // Add a new source file and remove an existing one.
    write(
        tmp.path(),
        "src/routes/orders.rs",
        "pub struct OrdersService;\n",
    );
    fs::remove_file(tmp.path().join("src/lib/helpers.ts")).unwrap();
    touch_after_snapshot(
        &tmp.path().join("src/routes/orders.rs"),
        snapshot_max_mtime_ms,
    );

    let scan = scan_incremental(&token, None, ScanProjectOptions::default());
    assert!(scan.delta.added.iter().any(|p| p == "src/routes/orders.rs"));
    assert!(scan.delta.removed.iter().any(|p| p == "src/lib/helpers.ts"));
    let result = &scan.result;
    let new_paths: Vec<&str> = result
        .files
        .iter()
        .map(|f| f.relative_path.as_str())
        .collect();
    assert!(new_paths.contains(&"src/routes/orders.rs"));
    assert!(!new_paths.contains(&"src/lib/helpers.ts"));
}

#[test]
fn scan_incremental_full_rescan_when_snapshot_missing() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let token = tmp.path().to_string_lossy().into_owned();
    let scan = scan_incremental(&token, None, ScanProjectOptions::default());
    assert!(scan.delta.full_rescan);
    assert!(!scan.result.files.is_empty());
}

#[test]
fn scan_incremental_modifies_via_explicit_changed_paths() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let initial = scan_project(tmp.path(), ScanProjectOptions::default());
    let token = initial.snapshot_token;

    // Replace the contents of an existing file.
    write(
        tmp.path(),
        "src/main.rs",
        "pub fn entry() { println!(\"refreshed\"); }\n",
    );

    let scan = scan_incremental(
        &token,
        Some(&["src/main.rs".to_string()]),
        ScanProjectOptions::default(),
    );
    assert!(scan.delta.modified.iter().any(|p| p == "src/main.rs"));
    let main = find_file(&scan.result.files, "src/main.rs").unwrap();
    assert!(main.imports.is_empty());
}

fn find_file<'a>(files: &'a [FileRecord], path: &str) -> Option<&'a FileRecord> {
    files.iter().find(|f| f.relative_path == path)
}

#[test]
fn scan_project_truncates_to_max_files() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let opts = ScanProjectOptions {
        max_files: 2,
        ..ScanProjectOptions::default()
    };
    let result = scan_project(tmp.path(), opts);
    assert!(result.truncated);
    assert!(result.files.len() <= 2);
}

#[test]
fn symbol_records_carry_canonical_kind() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());

    let result = scan_project(tmp.path(), ScanProjectOptions::default());
    let app = result.symbols.iter().find(|s| s.name == "App").unwrap();
    assert_eq!(app.kind, SymbolKind::StructDecl);
    let entry = result.symbols.iter().find(|s| s.name == "entry").unwrap();
    assert_eq!(entry.kind, SymbolKind::Function);
}

#[test]
fn scan_project_workspace_shape_smoke_test() {
    let tmp = tempdir().unwrap();
    build_fixture(tmp.path());
    write(
        tmp.path(),
        ".github/workflows/ci.yml",
        "name: ci\non: [push]\njobs:\n  test:\n    runs-on: ubuntu-latest\n",
    );
    write(
        tmp.path(),
        "docs/src/language-spec.md",
        "# Language Spec\n\n```harn\nfn main() { 1 }\n```\n",
    );
    write(
        tmp.path(),
        "crates/harn-hostlib/src/lib.rs",
        "pub mod scanner;\npub fn install() {}\n",
    );

    let result = scan_project_with_git(
        tmp.path(),
        ScanProjectOptions {
            include_git_history: false,
            ..ScanProjectOptions::default()
        },
        &MockGit::default(),
    );

    assert!(
        !result.files.is_empty(),
        "workspace fixture should have files"
    );
    assert!(
        !result.symbols.is_empty(),
        "workspace fixture should have symbols"
    );

    let names: Vec<&str> = result
        .files
        .iter()
        .map(|f| f.relative_path.as_str())
        .collect();
    assert!(names.iter().any(|n| n.ends_with("Cargo.toml")));
    assert!(names.iter().any(|n| n.contains("crates/harn-hostlib")));
}
