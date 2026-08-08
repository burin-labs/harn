//! End-to-end coverage for `harn canon check`: clap shim -> embedded Harn
//! handler -> std/agent/canon -> Flow invariant evaluation.

use std::fs;
use std::process::{Command, Output};

use tempfile::TempDir;

const INVARIANTS: &str = r#"
@invariant
@deterministic
@archivist(evidence: ["fixture"], confidence: 1.0, source_date: "2026-07-03")
pub fn no_bad(slice, _ctx, _repo_at_base) {
  if contains(slice.files[0].text, "bad") {
    return {
      verdict: "Block",
      rule: "no_bad",
      findings: [{path: slice.files[0].path, message: "bad sentinel"}],
      remediation: "Remove bad sentinel text.",
    }
  }
  return {verdict: "Allow", rule: "no_bad", findings: [], remediation: ""}
}
"#;

struct Outcome {
    stdout: String,
    stderr: String,
    code: i32,
}

fn harn(args: &[&str]) -> Outcome {
    let Output {
        status,
        stdout,
        stderr,
    } = Command::new(env!("CARGO_BIN_EXE_harn"))
        .args(args)
        .output()
        .expect("spawn harn");
    Outcome {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        code: status.code().unwrap_or(-1),
    }
}

fn fixture() -> TempDir {
    let temp = TempDir::new().expect("tempdir");
    fs::create_dir_all(temp.path().join("src")).expect("src dir");
    fs::create_dir_all(temp.path().join("zig")).expect("zig dir");
    fs::write(
        temp.path().join("canon-packs.json"),
        r#"{
  "schema_version": 1,
  "packs": [
    {
      "id": "zig",
      "title": "Zig",
      "invariants": "zig/invariants.harn",
      "extensions": ["foo"]
    }
  ]
}
"#,
    )
    .expect("manifest");
    fs::write(temp.path().join("zig/invariants.harn"), INVARIANTS).expect("invariants");
    fs::write(temp.path().join("src/main.foo"), "const bad = true;\n").expect("bad file");
    fs::write(temp.path().join("src/ok.foo"), "const good = true;\n").expect("ok file");
    temp
}

fn parse_json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout)
        .unwrap_or_else(|error| panic!("stdout is not JSON: {error}\n--- stdout ---\n{stdout}"))
}

#[test]
fn canon_check_json_reports_blocking_findings() {
    let temp = fixture();
    let root = temp.path().to_str().expect("utf8 temp path");
    let out = harn(&[
        "canon",
        "check",
        "src/main.foo",
        "--workspace-root",
        root,
        "--canon-root",
        root,
        "--json",
    ]);

    assert_eq!(out.code, 1, "stderr={}", out.stderr);
    let json = parse_json(&out.stdout);
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "canon_check_failed");
    assert_eq!(json["error"]["details"]["status"], "fail");
    assert_eq!(json["error"]["details"]["selected_pack_ids"][0], "zig");
    assert!(
        json["error"]["details"]["feedback_text"]
            .as_str()
            .unwrap_or("")
            .contains("no_bad"),
        "json={json}"
    );
}

#[test]
fn canon_check_advisory_human_output_exits_zero() {
    let temp = fixture();
    let root = temp.path().to_str().expect("utf8 temp path");
    let out = harn(&[
        "canon",
        "check",
        "src/main.foo",
        "--workspace-root",
        root,
        "--canon-root",
        root,
        "--advisory",
    ]);

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert!(out.stdout.contains("harn-canon check: fail"));
    assert!(out.stdout.contains("Flow invariants need attention"));
    assert!(out.stdout.contains("no_bad"));
}

#[test]
fn canon_check_passes_when_selected_pack_allows_slice() {
    let temp = fixture();
    let root = temp.path().to_str().expect("utf8 temp path");
    let out = harn(&[
        "canon",
        "check",
        "src/ok.foo",
        "--workspace-root",
        root,
        "--canon-root",
        root,
    ]);

    assert_eq!(out.code, 0, "stderr={}", out.stderr);
    assert!(out.stdout.contains("harn-canon check: pass"));
    assert!(out.stdout.contains("No harn-canon findings."));
}
