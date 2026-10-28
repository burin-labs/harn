//! `harn lint` and `harn check` analyze each target once.
//!
//! The process-wide counters behind `HARN_CHECK_ANALYSIS_STATS` count every
//! tokenization, parse, and type check the real binary runs. Before the
//! linter consumed the driver's type facts, it type-checked every program a
//! second time, without imports.

use std::fs;

use crate::test_util::process::run_harn_e2e as run;

struct Stats {
    files: usize,
    lex: usize,
    parse: usize,
    typecheck: usize,
}

fn stats(stderr: &str) -> Stats {
    let line = stderr
        .lines()
        .find_map(|line| line.strip_prefix("[harn] analysis stats: "))
        .unwrap_or_else(|| panic!("no analysis stats line in:\n{stderr}"));
    let field = |name: &str| -> usize {
        line.split_whitespace()
            .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("`{name}` missing from `{line}`"))
    };
    Stats {
        files: field("files"),
        lex: field("lex"),
        parse: field("parse"),
        typecheck: field("typecheck"),
    }
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("app.harn"),
        "import { helper } from \"./lib\"\n\npipeline main() {\n  const n = helper(2)\n  log(n)\n}\n",
    )
    .expect("write app");
    fs::write(
        dir.path().join("lib.harn"),
        "/** Doubles a value. */\npub fn helper(value: int) -> int {\n  return value * 2\n}\n",
    )
    .expect("write lib");
    fs::write(
        dir.path().join("other.harn"),
        "pipeline other() {\n  log(\"hi\")\n}\n",
    )
    .expect("write other");
    dir
}

#[test]
fn lint_parses_and_type_checks_each_target_once() {
    let dir = project();
    let target = dir.path().to_str().expect("UTF-8 path");
    let outcome = run(&["lint", target], &[("HARN_CHECK_ANALYSIS_STATS", "1")]);
    let stats = stats(&outcome.stderr);
    assert_eq!(stats.files, 3);
    assert_eq!(stats.parse, 3, "one parse per target: {}", outcome.stderr);
    assert_eq!(
        stats.typecheck, 3,
        "one type check per target: {}",
        outcome.stderr
    );
    // One lex for the parse, one comment-preserving lex shared by the
    // token-reading lint rules.
    assert_eq!(stats.lex, 6, "{}", outcome.stderr);
}

#[test]
fn check_type_checks_each_target_once() {
    let dir = project();
    let target = dir.path().to_str().expect("UTF-8 path");
    let outcome = run(
        &["check", target],
        &[
            ("HARN_CHECK_ANALYSIS_STATS", "1"),
            ("HARN_CHECK_RESULT_CACHE", "0"),
        ],
    );
    let stats = stats(&outcome.stderr);
    assert_eq!(stats.files, 3);
    assert_eq!(
        stats.typecheck, 3,
        "one type check per target: {}",
        outcome.stderr
    );
}

/// The linter reads the driver's import-aware type facts. Its own second pass
/// had no imports, so a binding typed by an imported declaration read as
/// unknown and type-aware rules stayed silent on it.
#[test]
fn lint_types_bindings_from_imported_declarations() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(
        dir.path().join("app.harn"),
        "import { ready } from \"./flags\"\n\npipeline main() {\n  const ok = ready()\n  if ok == true {\n    log(\"ready\")\n  }\n}\n",
    )
    .expect("write app");
    fs::write(
        dir.path().join("flags.harn"),
        "/** Whether the flag is set. */\npub fn ready() -> bool {\n  return true\n}\n",
    )
    .expect("write flags");
    let target = dir.path().to_str().expect("UTF-8 path");
    let outcome = run(&["lint", target], &[]);
    assert!(
        outcome.stderr.contains("comparison to `true` is redundant"),
        "the imported `bool` binding must reach the linter: {}",
        outcome.stderr
    );
}
