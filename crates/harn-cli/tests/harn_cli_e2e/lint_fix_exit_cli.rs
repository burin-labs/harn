//! `harn lint --fix` must fold residual diagnostics into its exit code just
//! like the plain and `--json` lint paths. An error-level diagnostic that is
//! unfixable — whether nothing was ever fixable, or it survives the applied
//! fixes — has to fail with exit 1 and still be printed. Otherwise CI/pre-commit
//! hooks running `--fix` pass green over real errors. Spawns the real `harn`
//! binary the test target built.
//!
//! Because these spawn the binary, they are also the only tests that observe
//! which OS stream a line lands on: every human-readable line goes to stderr
//! and stdout carries the `--json` envelope and nothing else (harn#6168). The
//! in-process tests assert the rendered buffer, which is the same buffer either
//! way, so the stdout assertions below are what hold that split in place.

use std::path::{Path, PathBuf};
use std::process::Command;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("harn-lint-fix-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir root");
    dir
}

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

// `break` outside a loop is HARN-LNT-039: an error-severity diagnostic with no
// autofix. Nothing here is fixable, so `--fix` exercises the "no edits" path.
const UNFIXABLE_ERROR_SOURCE: &str = "pipeline default(_) {\n  break\n}\n";

// `x == false` is HARN-LNT-032 (comparison-to-bool), a warning carrying an
// autofix — fully resolvable by `--fix`.
const FIXABLE_WARNING_SOURCE: &str =
    "pipeline default(_) {\n  const x = true\n  return x == false\n}\n";

// A fixable comparison-to-bool warning alongside an unfixable break-outside-loop
// error, so `--fix` applies edits and must still fail on what survives.
const FIXABLE_PLUS_ERROR_SOURCE: &str =
    "pipeline default(_) {\n  const x = true\n  const y = x == false\n  break\n  return y\n}\n";

#[test]
fn fix_fails_on_unfixable_error() {
    let dir = temp_root("unfixable-error");
    std::fs::write(dir.join("err.harn"), UNFIXABLE_ERROR_SOURCE).unwrap();
    let (_stdout, stderr, code) = run(&dir, &["lint", "--fix", "err.harn"]);
    assert_eq!(
        code, 1,
        "--fix must fail on an unfixable error-level diagnostic: {stderr}"
    );
    assert!(
        stderr.contains("error") && stderr.contains("break"),
        "--fix must still print the unfixable diagnostic:\n{stderr}"
    );
}

#[test]
fn fix_fails_on_error_surviving_applied_fix() {
    let dir = temp_root("survives-fix");
    std::fs::write(dir.join("mixed.harn"), FIXABLE_PLUS_ERROR_SOURCE).unwrap();
    let (stdout, stderr, code) = run(&dir, &["lint", "--fix", "mixed.harn"]);
    assert_eq!(
        code, 1,
        "an error surviving the applied fix must still fail --fix: {stderr}"
    );
    assert!(
        stderr.contains("applied"),
        "--fix should report the fixes it applied:\n{stderr}"
    );
    assert!(
        stderr.contains("break"),
        "--fix must print the diagnostic that survived the fix:\n{stderr}"
    );
    assert!(
        stdout.is_empty(),
        "no --json flag, so stdout must stay empty:\n{stdout}"
    );
}

#[test]
fn fix_passes_on_fully_fixable_file() {
    let dir = temp_root("fully-fixable");
    std::fs::write(dir.join("warn.harn"), FIXABLE_WARNING_SOURCE).unwrap();
    let (stdout, stderr, code) = run(&dir, &["lint", "--fix", "warn.harn"]);
    assert_eq!(
        code, 0,
        "--fix on a fully-fixable file must exit 0: {stderr}"
    );
    assert!(
        stderr.contains("applied"),
        "--fix should report the applied fix:\n{stderr}"
    );
    assert!(
        stdout.is_empty(),
        "no --json flag, so stdout must stay empty:\n{stdout}"
    );
}

#[test]
fn fix_resolves_warning_under_strict() {
    // Without `--fix`, `--strict` would fail on the comparison-to-bool warning;
    // once the autofix lands nothing remains, so the exit code is clean. This
    // proves the residual outcome — not the pre-fix state — governs `--fix`.
    let dir = temp_root("strict-fixable");
    std::fs::write(dir.join("warn.harn"), FIXABLE_WARNING_SOURCE).unwrap();
    let (_stdout, stderr, code) = run(&dir, &["lint", "--fix", "--strict", "warn.harn"]);
    assert_eq!(
        code, 0,
        "--fix must clear the warning so --strict passes: {stderr}"
    );
}

#[test]
fn fix_applies_prompt_template_filter_suggestion() {
    let dir = temp_root("prompt-filter");
    let path = dir.join("sample.harn.prompt");
    std::fs::write(&path, "{{ name | uppr }}\n").unwrap();

    let (stdout, stderr, code) = run(
        &dir,
        &["lint", "--fix", path.file_name().unwrap().to_str().unwrap()],
    );

    assert_eq!(code, 0, "--fix must apply the prompt fix: {stderr}");
    assert!(
        stderr.contains("applied 1 fix(es)"),
        "--fix should report the applied prompt fix:\n{stderr}"
    );
    assert!(
        stdout.is_empty(),
        "no --json flag, so stdout must stay empty:\n{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "{{ name | upper }}\n"
    );
}

#[test]
fn lint_and_fix_preserve_public_module_bindings() {
    let dir = temp_root("public-module-binding");
    let path = dir.join("public.harn");
    let source = "pub const EXPORTED_SETTING: string = \"configured\"\n";
    std::fs::write(&path, source).unwrap();

    let (_stdout, stderr, code) = run(&dir, &["lint", "public.harn"]);
    assert_eq!(code, 0, "public module API must lint cleanly: {stderr}");
    assert!(
        !stderr.contains("HARN-LNT-014") && !stderr.contains("unused-variable"),
        "public module API must not receive an unused diagnostic:\n{stderr}"
    );

    let (_stdout, stderr, code) = run(&dir, &["lint", "--fix", "public.harn"]);
    assert_eq!(
        code, 0,
        "public module API must remain clean under --fix: {stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        source,
        "--fix must not rename an externally reachable API to the discard binding"
    );
}
