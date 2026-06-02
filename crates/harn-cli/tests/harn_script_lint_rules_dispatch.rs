//! End-to-end coverage for `.harn`-authored custom lint rules (#2850): with a
//! `[rules] ruleDirs` in `harn.toml`, `harn lint` discovers `*.lint.harn`
//! modules, runs their `pub fn lint(source)` over each linted file, and merges
//! the findings into the normal lint output. A deliberately-buggy rule fails
//! safe. Spawns the real `harn` binary with its cwd set to the project root.

use std::path::{Path, PathBuf};
use std::process::Command;

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("harn-scriptlint-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("rules")).expect("mkdir rules");
    std::fs::create_dir_all(dir.join("src")).expect("mkdir src");
    std::fs::write(dir.join("harn.toml"), "[rules]\nruleDirs = [\"rules\"]\n").unwrap();
    dir
}

fn write(dir: &Path, rel: &str, contents: &str) {
    std::fs::write(dir.join(rel), contents).expect("write");
}

fn run(dir: &Path, args: &[&str]) -> (String, String, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("spawn harn");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// A `.harn` lint rule banning `TODO` markers — keys alphabetical (Harn dict
/// literal convention).
const TODO_RULE: &str = r#"pub fn lint(source) -> list {
  if source.contains("TODO") {
    return [{column: 1, line: 1, message: "TODO markers are banned", severity: "error"}]
  }
  return []
}
"#;

const CLEAN_SRC: &str = "pub fn greet() -> string {\n  return \"hi\"\n}\n";

#[test]
fn script_rule_flags_a_convention_violation() {
    let dir = project("flag");
    write(&dir, "rules/no-todo.lint.harn", TODO_RULE);
    write(
        &dir,
        "src/main.harn",
        "pub fn greet() -> string {\n  // TODO: rename\n  return \"hi\"\n}\n",
    );

    let (stdout, stderr, code) = run(&dir, &["lint", "src/main.harn"]);
    let all = format!("{stdout}{stderr}");
    assert_ne!(code, 0, "a violation must fail the lint: {all}");
    assert!(
        all.contains("TODO markers are banned"),
        "script-rule finding should surface: {all}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn script_rule_leaves_a_clean_file_clean() {
    let dir = project("clean");
    write(&dir, "rules/no-todo.lint.harn", TODO_RULE);
    write(&dir, "src/main.harn", CLEAN_SRC);

    let (stdout, stderr, code) = run(&dir, &["lint", "src/main.harn"]);
    let all = format!("{stdout}{stderr}");
    assert_eq!(code, 0, "a clean file must pass: {all}");
    assert!(
        !all.contains("TODO markers are banned"),
        "no finding expected on a clean file: {all}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn buggy_script_rule_fails_safe() {
    let dir = project("buggy");
    // A rule that throws at runtime must not crash the linter.
    write(
        &dir,
        "rules/explode.lint.harn",
        "pub fn lint(source) -> list {\n  throw \"deliberate rule bug\"\n}\n",
    );
    write(&dir, "src/main.harn", CLEAN_SRC);

    let (stdout, stderr, code) = run(&dir, &["lint", "src/main.harn"]);
    let all = format!("{stdout}{stderr}");
    // The linter completed (a real exit code, not a panic/abort) and reported
    // the rule failure as a diagnostic instead of crashing.
    assert!(
        code == 0 || code == 1,
        "linter must not crash on a buggy rule (got {code}): {all}"
    );
    assert!(
        all.contains("explode") && all.to_lowercase().contains("failed"),
        "a buggy rule should be reported as a failed diagnostic: {all}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn json_output_includes_script_rule_findings() {
    let dir = project("json");
    write(&dir, "rules/no-todo.lint.harn", TODO_RULE);
    write(
        &dir,
        "src/main.harn",
        "pub fn greet() -> string {\n  // TODO: rename\n  return \"hi\"\n}\n",
    );

    let (stdout, stderr, code) = run(&dir, &["lint", "src/main.harn", "--json"]);
    assert_ne!(code, 0, "violation must fail: stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    let blob = json.to_string();
    assert!(
        blob.contains("TODO markers are banned"),
        "json report should carry the script-rule finding: {blob}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
