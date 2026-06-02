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
