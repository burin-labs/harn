//! `harn lint --strict` promotes lint warnings to a non-zero exit so CI gates
//! can deny warning noise. Without `--strict`, warnings stay advisory (exit 0)
//! and only errors fail. A clean file passes under `--strict`. Spawns the real
//! `harn` binary that cargo already built for the test target.

use std::path::{Path, PathBuf};
use std::process::Command;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("harn-lint-strict-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir root");
    dir
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

// `x == false` triggers HARN-LNT-032 (comparison-to-bool), a warning with an
// autofix. Enough to exercise the warning-only path.
const WARNING_SOURCE: &str = "pipeline default(_) {\n  const x = true\n  return x == false\n}\n";

const CLEAN_SOURCE: &str = "pipeline default(_) {\n  const x = true\n  return !x\n}\n";

#[test]
fn warning_is_advisory_without_strict() {
    let dir = temp_root("advisory");
    std::fs::write(dir.join("warn.harn"), WARNING_SOURCE).unwrap();
    let (_stdout, stderr, code) = run(&dir, &["lint", "warn.harn"]);
    assert_eq!(
        code, 0,
        "default lint should not fail on a warning: {stderr}"
    );
}

#[test]
fn warning_fails_under_strict() {
    let dir = temp_root("strict-warn");
    std::fs::write(dir.join("warn.harn"), WARNING_SOURCE).unwrap();
    let (_stdout, stderr, code) = run(&dir, &["lint", "--strict", "warn.harn"]);
    assert_eq!(
        code, 1,
        "--strict should promote the warning to a failure: {stderr}"
    );
}

#[test]
fn clean_file_passes_under_strict() {
    let dir = temp_root("strict-clean");
    std::fs::write(dir.join("clean.harn"), CLEAN_SOURCE).unwrap();
    let (_stdout, stderr, code) = run(&dir, &["lint", "--strict", "clean.harn"]);
    assert_eq!(
        code, 0,
        "--strict must stay green on a clean file: {stderr}"
    );
}

#[test]
fn strict_json_reports_not_ok_on_warning() {
    let dir = temp_root("strict-json");
    std::fs::write(dir.join("warn.harn"), WARNING_SOURCE).unwrap();
    let (stdout, stderr, code) = run(&dir, &["lint", "--strict", "--json", "warn.harn"]);
    assert_eq!(
        code, 1,
        "--strict --json should fail on a warning: {stderr}"
    );
    assert!(
        stdout.contains("\"ok\": false"),
        "strict json envelope should report ok:false:\n{stdout}"
    );
}
