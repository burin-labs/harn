//! End-to-end coverage for `harn codemod` (#2841): dry-run by default (a
//! unified diff per file), `--apply` writes (gated), and re-running a folded
//! file is idempotent. Spawns the real `harn` binary.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Outcome {
    stdout: String,
    stderr: String,
    code: i32,
}

fn fixture_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("harn-codemod-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

const RENAME_RULE: &str = "id = \"rename\"\nlanguage = \"typescript\"\nsafety = \"behavior-preserving\"\nfix = \"bar()\"\n[rule]\npattern = \"foo()\"\n";

fn codemod(dir: &Path, extra: &[&str]) -> Outcome {
    let rule = dir.join("rule.toml");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harn"));
    cmd.arg("codemod")
        .arg("--rule")
        .arg(&rule)
        .arg(dir)
        .args(extra);
    let output = cmd.output().expect("spawn harn codemod");
    Outcome {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code().unwrap_or(-1),
    }
}

#[test]
fn dry_run_previews_without_writing() {
    let dir = fixture_dir("dry");
    write(&dir, "a.ts", "foo();\nconst keep = 1;\n");
    write(&dir, "rule.toml", RENAME_RULE);

    let out = codemod(&dir, &["--json"]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let json: serde_json::Value = serde_json::from_str(&out.stdout).expect("valid json");
    assert_eq!(json["mode"], "codemod");
    assert_eq!(json["apply"], false);
    assert_eq!(json["summary"]["changed"], 1);
    assert_eq!(json["summary"]["applied"], 0);
    assert_eq!(json["files"][0]["before"], "foo();\nconst keep = 1;\n");
    assert_eq!(json["files"][0]["preview"], "bar();\nconst keep = 1;\n");

    // The file on disk is untouched by a dry run.
    let on_disk = std::fs::read_to_string(dir.join("a.ts")).unwrap();
    assert_eq!(on_disk, "foo();\nconst keep = 1;\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn apply_writes_and_is_idempotent() {
    let dir = fixture_dir("apply");
    write(&dir, "a.ts", "foo();\nfoo();\n");
    write(&dir, "rule.toml", RENAME_RULE);

    let applied = codemod(&dir, &["--apply", "--json"]);
    assert_eq!(applied.code, 0, "stderr={}", applied.stderr);
    let json: serde_json::Value = serde_json::from_str(&applied.stdout).expect("valid json");
    assert_eq!(json["apply"], true);
    assert_eq!(json["summary"]["applied"], 1);

    let on_disk = std::fs::read_to_string(dir.join("a.ts")).unwrap();
    assert_eq!(on_disk, "bar();\nbar();\n");

    // Re-running the rule on the folded file changes nothing (idempotent).
    let again = codemod(&dir, &["--json"]);
    let json2: serde_json::Value = serde_json::from_str(&again.stdout).expect("valid json");
    assert_eq!(json2["summary"]["changed"], 0);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_rule_is_an_error() {
    let dir = fixture_dir("norule");
    let out = Command::new(env!("CARGO_BIN_EXE_harn"))
        .arg("codemod")
        .arg(&dir)
        .output()
        .expect("spawn");
    assert_ne!(out.status.code().unwrap_or(-1), 0);
    let _ = std::fs::remove_dir_all(&dir);
}
