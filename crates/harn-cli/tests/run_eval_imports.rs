//! End-to-end coverage for `harn run -e` with module imports.
//!
//! `-e` wraps the snippet in `pipeline main(task) { ... }`, but `import`
//! is a top-level Harn declaration so leading `import` lines are
//! hoisted out of the wrapper. The temp file backing `-e` is also
//! placed in the current working directory so relative imports resolve
//! against the user's project root rather than the system temp dir.

mod test_util;

use std::fs;

use tempfile::TempDir;
use test_util::process::harn_e2e_command;

#[ignore = "binary surface — moves to slow E2E/smoke job (issue #1069)"]
#[test]
fn eval_supports_stdlib_import() {
    let temp = TempDir::new().unwrap();
    let out = harn_e2e_command()
        .current_dir(temp.path())
        .args(["run", "-e", "import \"std/triggers\"\nprintln(\"ok\")"])
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
        .args(["run", "-e", "import \"./lib\"\nprintln(answer())"])
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
        .args(["run", "-e", "__io_println(1 + 2)"])
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
