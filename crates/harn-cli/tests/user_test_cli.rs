//! End-to-end coverage for user `harn test` runner output.

use std::process::Command;

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

#[test]
fn user_tests_emit_progress_and_timing_summary() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let suite = temp.path().join("suite");
    std::fs::create_dir_all(&suite).expect("create suite");
    std::fs::write(
        suite.join("test_alpha.harn"),
        r#"
pipeline test_alpha(task) {
  assert_eq(1, 1)
}
"#,
    )
    .expect("write alpha");
    std::fs::write(
        suite.join("test_beta.harn"),
        r#"
pipeline test_beta(task) {
  assert_eq(2, 2)
}
"#,
    )
    .expect("write beta");

    let output = Command::new(binary_path())
        .args([
            "test",
            suite.to_str().unwrap(),
            "--verbose",
            "--timing",
            "--timeout",
            "10000",
        ])
        .output()
        .expect("spawn harn test");

    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Running 2 tests from 2 files (sequential)"));
    assert!(stdout.contains("RUN   file 1/2"));
    assert!(stdout.contains("RUN   test_alpha"));
    assert!(stdout.contains("PASS"));
    assert!(stdout.contains("Slowest 2 tests:"));
    assert!(stdout.contains("Slowest 2 files:"));

    let first_file = stdout.find("RUN   file 1/2").expect("file progress");
    let first_pass = stdout.find("PASS").expect("pass output");
    assert!(
        first_file < first_pass,
        "file progress should be emitted before completed test results:\n{stdout}"
    );
}
