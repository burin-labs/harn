//! End-to-end coverage for `harn scan` (#2840): the embedded `.harn` handler
//! and clap shim run the rule engine over a fileset and emit human / `--json`
//! output. Spawns the real `harn` binary, like the other `*_dispatch` tests.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Outcome {
    stdout: String,
    #[allow(dead_code)]
    stderr: String,
    code: i32,
}

fn fixture_dir(name: &str) -> PathBuf {
    // A unique-per-test dir under the target tmp area.
    let dir = std::env::temp_dir().join(format!("harn-scan-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create fixture dir");
    dir
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write fixture");
}

fn scan(args: &[&str]) -> Outcome {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .arg("scan")
        .args(args)
        .output()
        .expect("spawn harn scan");
    Outcome {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        code: output.status.code().unwrap_or(-1),
    }
}

#[test]
fn inline_pattern_search_reports_matches_and_skips_other_languages() {
    let dir = fixture_dir("inline");
    write(
        &dir,
        "a.ts",
        "const a = cfg?.timeout ?? 30;\nconst b = opts?.retries ?? 3;\nconst c = plain;\n",
    );
    write(&dir, "b.ts", "let x = o?.count ?? 0;\n");
    // A Rust file with the same textual shape must be ignored (language filter).
    write(&dir, "c.rs", "let y = o.count;\n");

    let out = scan(&[
        "$X?.$K ?? $D",
        dir.to_str().unwrap(),
        "--lang",
        "typescript",
        "--json",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);

    let json: serde_json::Value = serde_json::from_str(&out.stdout).expect("valid json envelope");
    assert_eq!(json["mode"], "search");
    assert_eq!(json["summary"]["total"], 3);
    assert_eq!(json["summary"]["files"], 2);
    let captures = &json["results"][0]["matches"][0]["captures"];
    assert_eq!(captures["X"], "cfg");
    assert_eq!(captures["K"], "timeout");
    assert_eq!(captures["D"], "30");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inline_pattern_search_supports_harn_sources() {
    let dir = fixture_dir("harn");
    write(
        &dir,
        "rule_targets.harn",
        "fn main() {\n  let timeout = cfg?.timeout ?? 30\n  let retries = opts?.retries ?? 3\n}\n",
    );
    write(&dir, "other.ts", "const timeout = cfg?.timeout ?? 30;\n");

    let out = scan(&[
        "$X?.$K ?? $D",
        dir.to_str().unwrap(),
        "--lang",
        "harn",
        "--json",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);

    let json: serde_json::Value = serde_json::from_str(&out.stdout).expect("valid json envelope");
    assert_eq!(json["mode"], "search");
    assert_eq!(json["summary"]["total"], 2);
    assert_eq!(json["summary"]["files"], 1);
    let captures = &json["results"][0]["matches"][0]["captures"];
    assert_eq!(captures["X"], "cfg");
    assert_eq!(captures["K"], "timeout");
    assert_eq!(captures["D"], "30");

    write(
        &dir,
        "harn_default_rule.toml",
        "id = \"harn-defaults\"\nlanguage = \"harn\"\n[rule]\npattern = \"$X?.$K ?? $D\"\n",
    );
    let rule_out = scan(&[
        "--rule",
        dir.join("harn_default_rule.toml").to_str().unwrap(),
        dir.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(rule_out.code, 0, "stderr={}", rule_out.stderr);
    let rule_json: serde_json::Value =
        serde_json::from_str(&rule_out.stdout).expect("valid rule json envelope");
    assert_eq!(rule_json["mode"], "search");
    assert_eq!(rule_json["summary"]["total"], 2);
    assert_eq!(rule_json["summary"]["files"], 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn saved_harn_rule_surfaces_resolved_capture_metadata() {
    let dir = fixture_dir("harn-semantic");
    write(
        &dir,
        "calls.harn",
        "fn target(value: int) -> int {\n  return value\n}\n\nfn call_shadowed(target: fn(int) -> int) {\n  target(1)\n}\n\nfn call_global() {\n  target(2)\n}\n",
    );
    write(
        &dir,
        "target_call.toml",
        "id = \"target-call\"\nlanguage = \"harn\"\n[rule]\npattern = \"$FN($ARG)\"\n\n[[where]]\nmetavar = \"FN\"\nresolvesTo = { name = \"target\", kind = \"fn\", line = 1 }\n",
    );

    let out = scan(&[
        "--rule",
        dir.join("target_call.toml").to_str().unwrap(),
        dir.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let json: serde_json::Value = serde_json::from_str(&out.stdout).expect("valid json");
    assert_eq!(json["summary"]["total"], 1);
    let matched = &json["results"][0]["matches"][0];
    assert_eq!(matched["text"], "target(2)");
    assert_eq!(matched["captures"]["FN"], "target");
    assert_eq!(matched["capture_metadata"]["FN"]["type"], "fn(int) -> int");
    assert_eq!(
        matched["capture_metadata"]["FN"]["resolved"]["name"],
        "target"
    );
    assert_eq!(matched["capture_metadata"]["FN"]["resolved"]["kind"], "fn");
    assert_eq!(
        matched["capture_metadata"]["FN"]["resolved"]["start_row"],
        0
    );
    assert_eq!(matched["capture_metadata"]["ARG"]["type"], "int");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn report_only_emits_per_file_counts() {
    let dir = fixture_dir("report");
    write(&dir, "a.ts", "let p = a?.x ?? 1;\nlet q = b?.y ?? 2;\n");
    write(&dir, "b.ts", "let r = c?.z ?? 3;\n");

    let out = scan(&[
        "$X?.$K ?? $D",
        dir.to_str().unwrap(),
        "--lang",
        "typescript",
        "--report-only",
        "--json",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let json: serde_json::Value = serde_json::from_str(&out.stdout).expect("valid json");
    assert_eq!(json["mode"], "report");
    assert_eq!(json["summary"]["total"], 3);
    assert_eq!(json["summary"]["files"], 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rule_file_runs_a_saved_rule() {
    let dir = fixture_dir("rulefile");
    write(&dir, "calls.ts", "foo();\nbar();\nconst keep = 1;\n");
    write(
        &dir,
        "find_calls.toml",
        "id = \"find-calls\"\nlanguage = \"typescript\"\n[rule]\npattern = \"$FN()\"\n",
    );

    let out = scan(&[
        "--rule",
        dir.join("find_calls.toml").to_str().unwrap(),
        dir.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    let json: serde_json::Value = serde_json::from_str(&out.stdout).expect("valid json");
    assert_eq!(json["summary"]["total"], 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn inline_pattern_without_lang_is_an_error() {
    let dir = fixture_dir("nolang");
    write(&dir, "a.ts", "let p = a?.x ?? 1;\n");
    let out = scan(&["$X?.$K ?? $D", dir.to_str().unwrap()]);
    assert_ne!(out.code, 0, "missing --lang should fail");
    assert!(
        out.stderr.contains("--lang"),
        "stderr should mention --lang: {}",
        out.stderr
    );
    let _ = std::fs::remove_dir_all(&dir);
}
