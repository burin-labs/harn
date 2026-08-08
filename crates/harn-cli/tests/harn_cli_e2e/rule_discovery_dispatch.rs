//! End-to-end coverage for project rule discovery (#2843): with a
//! `[rules] ruleDirs` in `harn.toml`, `harn scan` / `harn codemod` load rules
//! from those directories when no `--rule`/`--rule-pack` is given. Spawns the
//! real `harn` binary with its cwd set to the project root.

use std::path::{Path, PathBuf};
use std::process::Command;

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("harn-discover-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("rules")).expect("mkdir rules");
    std::fs::create_dir_all(dir.join("src")).expect("mkdir src");
    std::fs::write(dir.join("harn.toml"), "[rules]\nruleDirs = [\"rules\"]\n").unwrap();
    dir
}

fn write(dir: &Path, rel: &str, contents: &str) {
    std::fs::write(dir.join(rel), contents).expect("write");
}

const LINT: &str =
    "id = \"no-foo\"\nlanguage = \"typescript\"\nmessage = \"no foo\"\n[rule]\npattern = \"foo()\"\n";
const CODEMOD: &str =
    "id = \"rename\"\nlanguage = \"typescript\"\nfix = \"bar()\"\n[rule]\npattern = \"foo()\"\n";

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

#[test]
fn scan_discovers_rules_from_project_ruledirs() {
    let dir = project("scan");
    write(&dir, "rules/no-foo.toml", LINT);
    write(&dir, "src/a.ts", "foo();\nbar();\nfoo();\n");

    // No --rule and no --lang: discovery mode, `src` is a path.
    let (stdout, stderr, code) = run(&dir, &["scan", "src", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(json["summary"]["total"], 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn codemod_discovery_skips_lints_and_applies_codemods() {
    let dir = project("codemod");
    write(&dir, "rules/lint.toml", LINT);
    write(&dir, "rules/cm.toml", CODEMOD);
    write(&dir, "src/a.ts", "foo();\n");

    // Mixed pack: the lint rule is skipped, the codemod rule applies.
    let (stdout, stderr, code) = run(&dir, &["codemod", "src", "--json"]);
    assert_eq!(code, 0, "stderr={stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(json["summary"]["changed"], 1);
    assert_eq!(json["files"][0]["preview"], "bar();\n");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inline_pattern_still_works_with_lang() {
    let dir = project("inline");
    write(&dir, "src/a.ts", "const x = a?.b ?? 1;\n");

    // `--lang` present → inline mode: the first positional is the pattern.
    let (stdout, _stderr, code) = run(
        &dir,
        &["scan", "$X?.$K ?? $D", "src", "--lang", "typescript"],
    );
    assert_eq!(code, 0);
    assert!(stdout.contains("1 match"), "stdout={stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn no_rule_and_no_config_is_a_helpful_error() {
    let dir = project("noconfig");
    std::fs::remove_file(dir.join("harn.toml")).unwrap();
    write(&dir, "src/a.ts", "foo();\n");

    let (_stdout, stderr, code) = run(&dir, &["scan", "src"]);
    assert_ne!(code, 0);
    assert!(stderr.contains("ruleDirs"), "stderr={stderr}");

    let _ = std::fs::remove_dir_all(&dir);
}
