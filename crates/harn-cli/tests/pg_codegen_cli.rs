//! CLI smoke tests for `harn pg codegen`.
//!
//! Spawns the binary to exercise the real clap parser and file IO. The DDL
//! parsing and type emission are covered by unit tests in
//! `commands::pg_codegen::tests`, so this file focuses on the CLI surface:
//! stdout vs `--out`, and the `--check` drift gate.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

/// Create a migrations directory under a unique temp path and return it.
fn migrations_dir(label: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "harn-pg-codegen-{label}-{}-{}",
        std::process::id(),
        // Monotonic-enough disambiguator without depending on wall-clock time.
        files.len()
    ));
    let dir = root.join("migrations");
    fs::create_dir_all(&dir).expect("create migrations dir");
    for (name, body) in files {
        fs::write(dir.join(name), body).expect("write migration");
    }
    root
}

#[test]
fn codegen_to_stdout_emits_record_type() {
    let root = migrations_dir(
        "stdout",
        &[(
            "0001_init.sql",
            "CREATE TABLE receipts (id BIGSERIAL PRIMARY KEY, note TEXT);",
        )],
    );
    let output = Command::new(binary_path())
        .args(["pg", "codegen", "--dir"])
        .arg(root.join("migrations"))
        .output()
        .expect("spawn harn pg codegen");
    assert!(output.status.success(), "exit={:?}", output.status.code());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("type ReceiptsRow = {"), "got:\n{stdout}");
    assert!(stdout.contains("id: int,"), "got:\n{stdout}");
    assert!(stdout.contains("note: string?,"), "got:\n{stdout}");
    fs::remove_dir_all(&root).ok();
}

#[test]
fn codegen_out_then_check_round_trips() {
    let root = migrations_dir(
        "check",
        &[(
            "0001_init.sql",
            "CREATE TABLE accounts (id INT NOT NULL, email TEXT NOT NULL);",
        )],
    );
    let dir = root.join("migrations");
    let out = root.join("types.harn");

    let write = Command::new(binary_path())
        .args(["pg", "codegen", "--dir"])
        .arg(&dir)
        .arg("--out")
        .arg(&out)
        .output()
        .expect("spawn write");
    assert!(
        write.status.success(),
        "write exit={:?}",
        write.status.code()
    );
    assert!(out.exists(), "expected generated file");

    let check_ok = Command::new(binary_path())
        .args(["pg", "codegen", "--dir"])
        .arg(&dir)
        .arg("--out")
        .arg(&out)
        .arg("--check")
        .output()
        .expect("spawn check");
    assert!(
        check_ok.status.success(),
        "fresh file should pass --check, exit={:?}",
        check_ok.status.code()
    );

    // Mutate the file: --check must now fail.
    let mut contents = fs::read_to_string(&out).expect("read out");
    contents.push_str("// drift\n");
    fs::write(&out, contents).expect("tamper");
    let check_drift = Command::new(binary_path())
        .args(["pg", "codegen", "--dir"])
        .arg(&dir)
        .arg("--out")
        .arg(&out)
        .arg("--check")
        .output()
        .expect("spawn check drift");
    assert!(
        !check_drift.status.success(),
        "tampered file should fail --check"
    );
    fs::remove_dir_all(&root).ok();
}

#[test]
fn check_requires_out() {
    let output = Command::new(binary_path())
        .args(["pg", "codegen", "--dir", "migrations", "--check"])
        .output()
        .expect("spawn harn pg codegen --check");
    assert!(
        !output.status.success(),
        "--check without --out should be rejected by clap"
    );
}
