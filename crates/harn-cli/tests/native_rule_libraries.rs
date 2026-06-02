//! End-to-end coverage for native lint rule libraries. Builds a small cdylib
//! rule fixture, points `[rules] nativeRuleDirs` at it, then runs the
//! already-built `harn` binary to prove the rule loads without rebuilding Harn.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/native_lint_rule")
}

fn dylib_filename() -> String {
    format!(
        "{}harn_native_lint_sample.{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_EXTENSION
    )
}

fn build_sample_rule() -> (tempfile::TempDir, PathBuf) {
    let target = tempfile::tempdir().expect("target tempdir");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .arg("build")
        .arg("--quiet")
        .arg("--locked")
        .current_dir(fixture_dir())
        .env("CARGO_TARGET_DIR", target.path())
        .env("CARGO_INCREMENTAL", "0")
        .output()
        .expect("build native rule fixture");
    assert!(
        output.status.success(),
        "native fixture build failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let dylib = target.path().join("debug").join(dylib_filename());
    assert!(
        dylib.exists(),
        "missing fixture dylib at {}",
        dylib.display()
    );
    (target, dylib)
}

fn run_harn(project: &Path, args: &[&str]) -> (String, String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .current_dir(project)
        .args(args)
        .output()
        .expect("run harn");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

#[test]
fn native_rule_library_reports_in_json_and_applies_fix() {
    let (_target, dylib) = build_sample_rule();
    let project = tempfile::tempdir().expect("project tempdir");
    let native_dir = project.path().join("native-rules");
    let src_dir = project.path().join("src");
    std::fs::create_dir_all(&native_dir).expect("mkdir native rules");
    std::fs::create_dir_all(&src_dir).expect("mkdir src");
    std::fs::copy(&dylib, native_dir.join(dylib_filename())).expect("copy dylib");
    std::fs::write(
        project.path().join("harn.toml"),
        "[rules]\nnativeRuleDirs = [\"native-rules\"]\n",
    )
    .expect("write manifest");
    let source_path = src_dir.join("main.harn");
    std::fs::write(
        &source_path,
        "fn main() -> int {\n  return 0\n}\n\n/* NATIVE_TODO */\n",
    )
    .expect("write harn source");

    let (_stdout, stderr, code) = run_harn(project.path(), &["lint", "src/main.harn"]);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stderr.contains("lint[native-no-todo]"), "stderr={stderr}");
    assert!(
        stderr.contains("native rule markers must be resolved"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("native node hook saw return 0"),
        "stderr={stderr}"
    );

    let (stdout, stderr, code) = run_harn(project.path(), &["lint", "--json", "src/main.harn"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let envelope: serde_json::Value = serde_json::from_str(&stdout).expect("json envelope");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["summary"]["diagnostics"], 2);
    let messages: Vec<&str> = envelope["data"]["files"][0]["diagnostics"]
        .as_array()
        .expect("diagnostics array")
        .iter()
        .map(|diagnostic| diagnostic["message"].as_str().expect("diagnostic message"))
        .collect();
    assert!(messages.contains(&"native rule markers must be resolved"));
    assert!(messages.contains(&"native node hook saw return 0"));

    let (_stdout, stderr, code) = run_harn(project.path(), &["lint", "--fix", "src/main.harn"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let fixed = std::fs::read_to_string(&source_path).expect("read fixed source");
    assert!(fixed.contains("NATIVE_DONE"), "fixed source={fixed}");
    assert!(!fixed.contains("NATIVE_TODO"), "fixed source={fixed}");
}
