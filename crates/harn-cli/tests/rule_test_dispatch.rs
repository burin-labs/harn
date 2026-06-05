//! End-to-end coverage for `harn rule test` (#2842): run a rule's annotated
//! fixture and pass/fail on whether matches line up with `// ruleid:` / `// ok:`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("harn-ruletest-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

const NO_FOO: &str = "id = \"no-foo\"\nlanguage = \"typescript\"\nmessage = \"no foo\"\n[rule]\npattern = \"foo()\"\n";

fn rule_test(dir: &Path, extra: &[&str]) -> (String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_harn"))
        .arg("rule")
        .arg("test")
        .arg(dir)
        .args(extra)
        .output()
        .expect("spawn harn rule test");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn no_path_discovers_project_ruledirs() {
    // With no path, `harn rule test` uses the project's `[rules] ruleDirs`
    // (#2843) — scoped to the declared dirs, so a stray non-rule `*.toml`
    // elsewhere in the project is not swept up.
    let dir = fixture_dir("discover");
    std::fs::create_dir_all(dir.join("rules")).unwrap();
    write(&dir, "harn.toml", "[rules]\nruleDirs = [\"rules\"]\n");
    write(&dir, "rules/no-foo.toml", NO_FOO);
    write(
        &dir,
        "rules/no-foo.ts",
        "// ruleid: no-foo\nfoo();\n// ok: no-foo\nbar();\n",
    );
    // A non-rule TOML at the project root that must be ignored.
    write(&dir, "other.toml", "[package]\nname = \"x\"\n");

    let out = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(&dir)
        .args(["rule", "test"])
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code().unwrap_or(-1), 0, "stdout={stdout}");
    assert!(stdout.contains("1 passed"), "stdout={stdout}");
    assert!(!stdout.contains("other.toml"), "stray toml swept: {stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_path_validates_project_ruledirs() {
    let dir = fixture_dir("missing-dir");
    write(&dir, "harn.toml", "[rules]\nruleDirs = [\"missing\"]\n");

    let out = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(&dir)
        .args(["rule", "test"])
        .output()
        .expect("spawn");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(out.status.code().unwrap_or(-1), 0, "stderr={stderr}");
    assert!(stderr.contains("not a directory"), "stderr={stderr}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_path_project_discovery_does_not_sweep_nested_utility_dirs() {
    let dir = fixture_dir("nested-util");
    std::fs::create_dir_all(dir.join("rules/util")).unwrap();
    write(
        &dir,
        "harn.toml",
        "[rules]\nruleDirs = [\"rules\"]\nutilDirs = [\"rules/util\"]\n",
    );
    write(&dir, "rules/no-foo.toml", NO_FOO);
    write(
        &dir,
        "rules/no-foo.ts",
        "// ruleid: no-foo\nfoo();\n// ok: no-foo\nbar();\n",
    );
    write(
        &dir,
        "rules/util/helper.toml",
        "id = \"helper\"\nlanguage = \"typescript\"\n[rule]\npattern = \"helper()\"\n",
    );

    let out = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(&dir)
        .args(["rule", "test"])
        .output()
        .expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("1 passed"), "stdout={stdout}");
    assert!(
        !stdout.contains("helper"),
        "nested utility rule should not be tested: {stdout}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn passing_fixture_exits_zero() {
    let dir = fixture_dir("pass");
    write(&dir, "no-foo.toml", NO_FOO);
    write(
        &dir,
        "no-foo.ts",
        "// ruleid: no-foo\nfoo();\n// ok: no-foo\nbar();\n",
    );

    let (stdout, code) = rule_test(&dir, &[]);
    assert_eq!(code, 0, "stdout={stdout}");
    assert!(stdout.contains("PASS"), "stdout={stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn false_positive_fails_with_exit_one() {
    let dir = fixture_dir("fail");
    write(&dir, "no-foo.toml", NO_FOO);
    // Two matches but only one annotation → the second is an un-annotated
    // false positive.
    write(&dir, "no-foo.ts", "// ruleid: no-foo\nfoo();\nfoo();\n");

    let (stdout, code) = rule_test(&dir, &["--json"]);
    assert_eq!(code, 1, "stdout={stdout}");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(json["passed"], false);
    assert_eq!(json["cases"][0]["matches"], 2);
    let _ = std::fs::remove_dir_all(&dir);
}
