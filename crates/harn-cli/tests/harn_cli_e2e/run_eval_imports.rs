//! End-to-end coverage for `harn run -e` with module imports.
//!
//! `-e` wraps the snippet in `pipeline main(harness: Harness, task) { ... }`, but `import`
//! is a top-level Harn declaration so leading `import` lines are
//! hoisted out of the wrapper. The temp file backing `-e` is also
//! placed in the current working directory so relative imports resolve
//! against the user's project root rather than the system temp dir.

use crate::test_util;

use std::fs;

use tempfile::TempDir;
use test_util::process::harn_e2e_command;

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn eval_supports_stdlib_import() {
    let temp = TempDir::new().unwrap();
    let out = harn_e2e_command()
        .current_dir(temp.path())
        .args([
            "run",
            "-e",
            "import \"std/triggers\"\nharness.stdio.println(\"ok\")",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("ok"),
        "stdout did not contain 'ok': {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn eval_supports_relative_import_against_cwd() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("lib.harn"),
        "pub fn answer() {\n  return 42\n}\n",
    )
    .unwrap();

    let out = harn_e2e_command()
        .current_dir(temp.path())
        .args([
            "run",
            "-e",
            "import \"./lib\"\nharness.stdio.println(answer())",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("42"),
        "stdout did not contain '42': {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn eval_pure_expression_still_works() {
    let temp = TempDir::new().unwrap();
    let out = harn_e2e_command()
        .current_dir(temp.path())
        .args(["run", "-e", "harness.stdio.println(1 + 2)"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains('3'),
        "stdout did not contain '3': {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn eval_pipeline_return_sets_exit_code() {
    let temp = TempDir::new().unwrap();
    let out = harn_e2e_command()
        .current_dir(temp.path())
        .args(["run", "-e", "return 7"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(7),
        "stdout={}, stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn eval_function_entrypoint_executes_imported_body() {
    let temp = TempDir::new().unwrap();
    fs::write(
        temp.path().join("lib.harn"),
        "pub fn answer() -> int { return 42 }",
    )
    .unwrap();
    let out = harn_e2e_command()
        .current_dir(temp.path())
        .args([
            "run", "-e",
            "import { answer } from \"./lib\"\nfn main(harness: Harness) { harness.stdio.println(answer()) }",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "42");
}

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn eval_function_entrypoint_failure_matches_file_execution() {
    let temp = TempDir::new().unwrap();
    let source = "fn main(harness: Harness) { throw \"entry body reached\" }";
    fs::write(temp.path().join("main.harn"), source).unwrap();
    for args in [vec!["run", "main.harn"], vec!["run", "-e", source]] {
        let out = harn_e2e_command()
            .current_dir(temp.path())
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(1),
            "stdout={}, stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("entry body reached"));
    }
}
