//! E2E coverage for `harn test --coverage`.
//!
//! Spawns the real binary so the clap surface, the coverage session wiring,
//! and the LCOV writer are exercised end to end. The line-accounting math
//! itself is unit-tested in `harn_vm::coverage`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tempfile::TempDir;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

#[test]
fn test_help_advertises_coverage_flags() {
    let output = Command::new(binary_path())
        .args(["test", "--help"])
        .output()
        .expect("spawn harn test --help");
    assert!(output.status.success(), "exit={:?}", output.status.code());
    let help = String::from_utf8_lossy(&output.stdout);
    for token in ["--coverage", "--coverage-out"] {
        assert!(
            help.contains(token),
            "expected `{token}` in `harn test --help`, got:\n{help}"
        );
    }
}

#[test]
fn coverage_summary_and_lcov_distinguish_hit_and_unhit_lines() {
    let temp = TempDir::new().unwrap();
    let script = temp.path().join("cov_fixture.harn");
    // `covered` runs via the test; `never_called` is compiled but never
    // executed, so its body line must report as an unhit `DA:<line>,0`.
    let source = "\
fn covered(x) {
  return x + 1
}

fn never_called(x) {
  return x - 1
}

pipeline test_covers_one(task) {
  assert_eq(covered(2), 3)
}
";
    fs::write(&script, source).unwrap();
    let lcov = temp.path().join("lcov.info");

    let output = Command::new(binary_path())
        .args([
            "test",
            script.to_str().unwrap(),
            "--coverage",
            "--coverage-out",
            lcov.to_str().unwrap(),
        ])
        .output()
        .expect("spawn harn test --coverage");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "harn test --coverage should pass; exit={:?}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        stdout.contains("Line coverage:"),
        "expected a coverage summary on stdout, got:\n{stdout}"
    );

    let tracefile = fs::read_to_string(&lcov).expect("LCOV tracefile written");
    assert!(
        tracefile.contains("cov_fixture.harn"),
        "LCOV should reference the fixture file:\n{tracefile}"
    );
    assert!(
        tracefile.contains(",1"),
        "LCOV should record at least one executed line:\n{tracefile}"
    );
    assert!(
        tracefile.contains(",0"),
        "LCOV should record the uncovered `never_called` body line:\n{tracefile}"
    );
    assert!(
        tracefile.contains("end_of_record"),
        "LCOV record should be terminated:\n{tracefile}"
    );
}
