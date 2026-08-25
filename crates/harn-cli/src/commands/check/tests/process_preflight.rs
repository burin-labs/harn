use super::*;

fn diagnostics(file: &std::path::Path, source: &str) -> Vec<PreflightDiagnostic> {
    let program = parse_program(source);
    collect_preflight_diagnostics(file, source, &program, &CheckConfig::default())
}

/// The execution-directory check must hold for the removed spelling, the
/// supported spelling, and an unrelated receiver with a colliding method.
#[test]
fn preflight_reports_a_missing_execution_dir_through_the_capability_method() {
    let dir = unique_temp_dir("harn-check-exec-at");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");

    let reported = |source: &str| {
        diagnostics(&file, source)
            .into_iter()
            .filter(|diagnostic| diagnostic.code.as_str() == "HARN-ORC-010")
            .collect::<Vec<_>>()
    };

    let legacy = reported("fn main(harness: Harness) {\n  exec_at(\"no-such-dir\", \"ls\")\n}\n");
    assert_eq!(legacy.len(), 1, "removed spelling: {legacy:?}");

    let capability = reported(
        "fn main(harness: Harness) {\n  harness.process.exec_at(\"no-such-dir\", \"ls\")\n}\n",
    );
    assert_eq!(capability.len(), 1, "capability spelling: {capability:?}");
    assert!(capability[0].message.contains("no-such-dir"));

    let shell = reported(
        "fn main(harness: Harness) {\n  harness.process.shell_at(\"no-such-dir\", \"ls\")\n}\n",
    );
    assert_eq!(shell.len(), 1, "shell_at: {shell:?}");

    let unrelated =
        reported("fn main(harness: Harness) {\n  client.exec_at(\"no-such-dir\", \"ls\")\n}\n");
    assert!(unrelated.is_empty(), "unrelated receiver: {unrelated:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Recognizing the capability spelling must not stop the recursive walk.
#[test]
fn preflight_keeps_scanning_inside_a_recognized_capability_call() {
    let dir = unique_temp_dir("harn-check-exec-at-nested");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.harn");
    let source = "fn main(harness: Harness) {\n  harness.process.exec_at(\"no-such-dir\", harness.fs.render_prompt(\"missing.txt\"))\n}\n";
    let diagnostics = diagnostics(&file, source);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code.as_str() == "HARN-ORC-010"),
        "outer call still reports: {diagnostics:?}"
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("render_prompt target")),
        "nested argument is still scanned: {diagnostics:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
