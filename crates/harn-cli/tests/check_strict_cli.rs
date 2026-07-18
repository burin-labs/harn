//! End-to-end contract for warning-fatal `harn check --strict`.

mod test_util;

use std::path::Path;
use std::process::Output;

fn run_check(root: &Path, args: &[&str], jobs: Option<&str>) -> Output {
    let mut command = test_util::process::harn_e2e_command();
    command
        .arg("check")
        .args(args)
        .current_dir(root)
        .env("HARN_CHECK_RESULT_CACHE", "0");
    if let Some(jobs) = jobs {
        command.env("HARN_CHECK_JOBS", jobs);
    }
    command.output().expect("run harn check")
}

fn warning_source(name: &str) -> String {
    format!("fn unused_{name}() {{\n  return 1\n}}\npipeline main(task) {{\n  return 1\n}}\n")
}

fn stdout_json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )
    })
}

fn assert_same_rendered_output(left: &Output, right: &Output, context: &str) {
    assert_eq!(
        String::from_utf8_lossy(&left.stdout),
        String::from_utf8_lossy(&right.stdout),
        "{context}: stdout drifted"
    );
    assert_eq!(
        String::from_utf8_lossy(&left.stderr),
        String::from_utf8_lossy(&right.stderr),
        "{context}: stderr drifted"
    );
}

#[test]
fn strict_changes_only_warning_exit_semantics_in_text_and_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("main.harn"), warning_source("text_json"))
        .expect("write source");

    let advisory = run_check(temp.path(), &["main.harn"], None);
    assert!(
        advisory.status.success(),
        "default warnings must stay advisory:\n{}",
        String::from_utf8_lossy(&advisory.stderr)
    );
    let strict = run_check(temp.path(), &["--strict", "main.harn"], None);
    assert!(!strict.status.success(), "--strict must fail on warnings");
    assert_same_rendered_output(&advisory, &strict, "text strictness");

    let advisory_json = run_check(temp.path(), &["--json", "main.harn"], None);
    assert!(advisory_json.status.success());
    let advisory_envelope = stdout_json(&advisory_json);
    assert_eq!(advisory_envelope["ok"], true);
    assert_eq!(advisory_envelope["data"]["summary"]["warnings"], 1);
    assert_eq!(advisory_envelope["data"]["files"][0]["status"], "warning");

    let strict_json = run_check(temp.path(), &["--strict", "--json", "main.harn"], None);
    assert!(!strict_json.status.success());
    let strict_envelope = stdout_json(&strict_json);
    assert_eq!(strict_envelope["ok"], false);
    assert_eq!(strict_envelope["error"]["code"], "check_failed");
    assert_eq!(strict_envelope["data"], advisory_envelope["data"]);
}

#[test]
fn strict_and_strict_types_compose_monotonically() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("main.harn"),
        warning_source("strict_types"),
    )
    .expect("write source");

    let strict_types_only = run_check(temp.path(), &["--strict-types", "main.harn"], None);
    assert!(
        strict_types_only.status.success(),
        "strict-types alone must preserve advisory warning semantics:\n{}",
        String::from_utf8_lossy(&strict_types_only.stderr)
    );
    let combined = run_check(
        temp.path(),
        &["--strict", "--strict-types", "main.harn"],
        None,
    );
    assert!(!combined.status.success());
    assert_same_rendered_output(&strict_types_only, &combined, "strict-types composition");
}

#[test]
fn strict_types_rejects_absent_selective_type_import_before_runtime() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        temp.path().join("compiler.harn"),
        "pub type PersonaBlueprintLowering = {ok: bool}\n",
    )
    .expect("write target module");
    std::fs::write(
        temp.path().join("main.harn"),
        "import { PersonaPromptCompileReceipt } from \"./compiler\"\n\npub fn grade(value: PersonaPromptCompileReceipt) -> bool {\n  return true\n}\n",
    )
    .expect("write consumer");

    let output = run_check(
        temp.path(),
        &["--strict-types", "--json", "main.harn"],
        None,
    );
    assert_eq!(output.status.code(), Some(1));
    let envelope = stdout_json(&output);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "check_failed");
    let diagnostics = envelope["data"]["files"][0]["diagnostics"]
        .as_array()
        .expect("diagnostics");
    let import_diagnostics = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "HARN-IMP-002")
        .collect::<Vec<_>>();
    assert_eq!(import_diagnostics.len(), 1);
    let diagnostic = import_diagnostics[0];
    assert_eq!(diagnostic["source"], "preflight");
    assert_eq!(diagnostic["severity"], "error");
    assert_eq!(
        diagnostic["message"],
        "imported symbol `PersonaPromptCompileReceipt` does not exist in `./compiler`"
    );
    assert_eq!(
        diagnostic["help"],
        "update the import to a symbol exported by `./compiler`"
    );
    assert_eq!(diagnostic["span"]["start"], 0);
    assert_eq!(diagnostic["span"]["end"], 56);
}

#[test]
fn manifest_strictness_cannot_be_disabled_by_cli_defaults() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("main.harn"), warning_source("manifest"))
        .expect("write source");
    std::fs::write(temp.path().join("harn.toml"), "[check]\nstrict = true\n")
        .expect("write manifest");

    let configured = run_check(temp.path(), &["main.harn"], None);
    assert!(
        !configured.status.success(),
        "[check] strict=true must fail on warnings"
    );
    let configured_and_cli = run_check(temp.path(), &["--strict", "main.harn"], None);
    assert!(!configured_and_cli.status.success());
    assert_same_rendered_output(
        &configured,
        &configured_and_cli,
        "config and CLI monotonicity",
    );
}

#[test]
fn strict_parallel_check_renders_every_file_deterministically() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("a.harn"), warning_source("a")).expect("write a");
    std::fs::write(temp.path().join("b.harn"), warning_source("b")).expect("write b");

    let serial = run_check(temp.path(), &["--strict", "."], Some("1"));
    let parallel = run_check(temp.path(), &["--strict", "."], Some("4"));
    assert!(!serial.status.success());
    assert!(!parallel.status.success());
    assert_same_rendered_output(&serial, &parallel, "serial/parallel strict aggregation");

    let rendered = format!(
        "{}{}",
        String::from_utf8_lossy(&parallel.stdout),
        String::from_utf8_lossy(&parallel.stderr)
    );
    assert!(rendered.contains("a.harn"), "first file was not rendered");
    assert!(rendered.contains("b.harn"), "second file was not rendered");
}
