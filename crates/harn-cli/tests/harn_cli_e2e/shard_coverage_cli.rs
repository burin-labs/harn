//! A green report is not evidence that the complete matrix ran.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::test_util::process::harn_e2e_command;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "Harn matrix fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Harn matrix fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

#[test]
fn verification_refuses_execution_flags_before_reading_reports() {
    for flags in [
        vec!["--junit", "unused.xml"],
        vec!["--fail-fast"],
        vec!["--verbose"],
        vec!["--timeout", "1000"],
        vec!["--jobs", "2"],
    ] {
        let output = harn_e2e_command()
            .args([
                "test",
                "--verify-shards",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "--shard-total",
                "1",
            ])
            .args(&flags)
            .arg("missing-report.json")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "flags={flags:?}");
        assert!(output.stdout.is_empty(), "verification must not run");
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
    }
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("suite/a")).unwrap();
    std::fs::create_dir_all(root.path().join("reports")).unwrap();
    std::fs::write(
        root.path().join(".gitignore"),
        "reports/\n.harn*/\n.config-home/\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("suite/a/test_group.harn"),
        r"
@heavy(threads: 3)
pipeline test_heavy() { assert_eq(1, 1) }
pipeline test_b() { assert_eq(2, 2) }
",
    )
    .unwrap();
    std::fs::write(
        root.path().join("suite/a0.harn"),
        r"
pipeline test_c() { assert_eq(3, 3) }
pipeline test_d() { assert_eq(4, 4) }
",
    )
    .unwrap();
    std::fs::write(
        root.path().join("suite/test_cases.harn"),
        r"
pipeline test_e() { assert_eq(5, 5) }
pipeline test_f() { assert_eq(6, 6) }
",
    )
    .unwrap();
    git(root.path(), &["init", "-q"]);
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "matrix fixture"]);
    root
}

fn shard(root: &Path, index: usize, total: usize, workers: usize, name: &str) -> Value {
    let path = format!("reports/{name}.json");
    let output = harn_e2e_command()
        .current_dir(root)
        .args([
            "test",
            "suite",
            "--parallel",
            "--jobs",
            &workers.to_string(),
            "--shard-index",
            &index.to_string(),
            "--shard-total",
            &total.to_string(),
            "--json-out",
            &path,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&std::fs::read(root.join(path)).unwrap()).unwrap()
}

fn verify(root: &Path, source: &str, total: usize, files: &[&str]) -> (bool, Value) {
    let output = harn_e2e_command()
        .current_dir(root)
        .args([
            "test",
            "--verify-shards",
            source,
            "--shard-total",
            &total.to_string(),
        ])
        .args(files)
        .output()
        .unwrap();
    let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.success(), value)
}

fn save(root: &Path, value: &Value) {
    std::fs::write(
        root.join("reports/changed.json"),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}

#[test]
fn shard_coverage_requires_exactly_once_cases_from_one_measured_source_and_plan() {
    let root = fixture();
    let source = git(root.path(), &["rev-parse", "HEAD"]);
    let mut reports = Vec::new();
    for (index, workers) in [(1, 1), (2, 2), (3, 3)] {
        reports.push(shard(root.path(), index, 3, workers, &index.to_string()));
    }
    let files = ["reports/1.json", "reports/2.json", "reports/3.json"];
    let (passed, verdict) = verify(root.path(), &source, 3, &files);
    assert!(passed, "{verdict}");
    assert_eq!(verdict["planned_cases"], 6);
    assert_eq!(verdict["unique_cases"], 6);
    assert_eq!(verdict["observed_cases"], 6);
    let plan = reports[0]["shard_plan"]["matrix"]["cases"]
        .as_array()
        .unwrap();
    let keys = plan
        .iter()
        .map(|case| {
            (
                case["file"].as_str().unwrap(),
                case["name"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        keys.windows(2).all(|pair| pair[0] <= pair[1]),
        "wire inventory must sort portable paths"
    );
    assert!(keys.iter().any(|(file, _)| file.starts_with("a/")));
    assert!(keys.iter().any(|(file, _)| *file == "a0.harn"));
    assert!(verdict["plan_identity"]
        .as_str()
        .unwrap()
        .starts_with("blake3:"));

    let (passed, missing) = verify(root.path(), &source, 3, &files[..2]);
    assert!(!passed);
    assert_eq!(missing["missing_shards"], serde_json::json!([3]));
    assert!(!missing["missing_cases"].as_array().unwrap().is_empty());
    let (passed, empty) = verify(root.path(), &source, 3, &[]);
    assert!(!passed);
    assert_eq!(empty["planned_cases"], 0);
    assert_eq!(empty["missing_shards"].as_array().unwrap().len(), 3);

    for variant in [
        "omitted",
        "duplicate",
        "source",
        "plan",
        "skipped",
        "unconfirmed",
    ] {
        let mut changed = reports[0].clone();
        match variant {
            "omitted" => {
                changed["cases"].as_array_mut().unwrap().remove(0);
                changed["summary"]["total"] =
                    (changed["summary"]["total"].as_u64().unwrap() - 1).into();
                changed["summary"]["passed"] =
                    (changed["summary"]["passed"].as_u64().unwrap() - 1).into();
            }
            "duplicate" => {
                let duplicate = changed["cases"][0].clone();
                changed["cases"].as_array_mut().unwrap().push(duplicate);
                changed["summary"]["total"] =
                    (changed["summary"]["total"].as_u64().unwrap() + 1).into();
                changed["summary"]["passed"] =
                    (changed["summary"]["passed"].as_u64().unwrap() + 1).into();
            }
            "source" => changed["shard_plan"]["matrix"]["source"]["commit"] = "b".repeat(40).into(),
            "plan" => changed["shard_plan"]["matrix"]["cases"][0]["declared_weight"] = 17.into(),
            "skipped" => {
                changed["cases"][0]["outcome"] = "skipped".into();
                changed["summary"]["passed"] =
                    (changed["summary"]["passed"].as_u64().unwrap() - 1).into();
                changed["summary"]["skipped"] = 1.into();
            }
            "unconfirmed" => changed["shard_plan"]["matrix"]["source_confirmed"] = false.into(),
            _ => unreachable!(),
        }
        save(root.path(), &changed);
        let (passed, refused) = verify(
            root.path(),
            &source,
            3,
            &["reports/changed.json", files[1], files[2]],
        );
        assert!(!passed, "{variant}: {refused}");
        match variant {
            "omitted" => assert_eq!(refused["missing_cases"].as_array().unwrap().len(), 1),
            "duplicate" => assert_eq!(refused["duplicate_cases"].as_array().unwrap().len(), 1),
            "skipped" => assert_eq!(refused["failed_cases"].as_array().unwrap().len(), 1),
            _ => assert!(!refused["errors"].as_array().unwrap().is_empty()),
        }
    }
    std::fs::write(
        root.path().join("reports/changed.json"),
        "{\"schemaVersion\":",
    )
    .unwrap();
    let (passed, partial) = verify(
        root.path(),
        &source,
        3,
        &["reports/changed.json", files[1], files[2]],
    );
    assert!(!passed);
    assert!(!partial["errors"].as_array().unwrap().is_empty());
    assert!(!partial["missing_cases"].as_array().unwrap().is_empty());

    // Reach the real measurement boundary with green executed tests over a
    // dirty checkout. Shape-only receipt edits above cannot substitute for it.
    std::fs::write(
        root.path().join("suite/test_cases.harn"),
        "pipeline test_dirty() { assert_eq(1, 1) }\n",
    )
    .unwrap();
    let dirty = shard(root.path(), 1, 1, 1, "dirty");
    assert!(dirty["shard_plan"]["matrix"]["source"].is_null());
    let (passed, refused) = verify(root.path(), &source, 1, &["reports/dirty.json"]);
    assert!(!passed, "{refused}");
}

#[test]
fn shard_coverage_does_not_confirm_source_changed_during_green_execution() {
    let root = fixture();
    std::fs::remove_dir_all(root.path().join("suite/a")).unwrap();
    std::fs::remove_file(root.path().join("suite/a0.harn")).unwrap();
    std::fs::write(
        root.path().join("suite/test_cases.harn"),
        r#"
pipeline test_mutation(harness: Harness) {
  harness.fs.write_text(path_join(harness.fs.runtime_paths().asset_root, "changed.txt"), "changed")
  assert_eq(1, 1)
}
"#,
    )
    .unwrap();
    git(root.path(), &["add", "."]);
    git(root.path(), &["commit", "-qm", "mutating test fixture"]);
    let source = git(root.path(), &["rev-parse", "HEAD"]);
    let report = shard(root.path(), 1, 1, 1, "mutating");
    assert_eq!(report["summary"]["passed"], 1);
    assert_eq!(report["shard_plan"]["matrix"]["source"]["commit"], source);
    assert_eq!(report["shard_plan"]["matrix"]["source_confirmed"], false);
    let (passed, refused) = verify(root.path(), &source, 1, &["reports/mutating.json"]);
    assert!(!passed, "{refused}");
}

#[test]
#[cfg(unix)]
fn shard_coverage_honors_normal_git_configuration() {
    use std::os::unix::fs::PermissionsExt;

    let root = fixture();
    git(root.path(), &["config", "--unset", "core.filemode"]);
    let source = git(root.path(), &["rev-parse", "HEAD"]);
    let home = root.path().join(".config-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join(".gitconfig"), "[core]\n  filemode = false\n").unwrap();
    let file = root.path().join("suite/test_cases.harn");
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
    let control = Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=normal"])
        .current_dir(root.path())
        .env("HOME", &home)
        .env_remove("GIT_CONFIG_GLOBAL")
        .output()
        .unwrap();
    assert!(control.status.success());
    assert!(
        control.stdout.is_empty(),
        "Git must measure the same clean checkout first: {}",
        String::from_utf8_lossy(&control.stdout)
    );
    let output = harn_e2e_command()
        .current_dir(root.path())
        .env("HOME", &home)
        .env_remove("GIT_CONFIG_GLOBAL")
        .args([
            "test",
            "suite",
            "--shard-index",
            "1",
            "--shard-total",
            "1",
            "--json-out",
            "reports/configured.json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(
        &std::fs::read(root.path().join("reports/configured.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(report["summary"]["passed"], 6);
    assert_eq!(report["shard_plan"]["matrix"]["source"]["commit"], source);
    assert_eq!(report["shard_plan"]["matrix"]["source_confirmed"], true);
    let (passed, verdict) = verify(root.path(), &source, 1, &["reports/configured.json"]);
    assert!(passed, "{verdict}");
}
