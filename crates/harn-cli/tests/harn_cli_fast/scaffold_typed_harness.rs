//! Fast projection guards for generated packages' typed Harness boundary.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn harn_bin() -> &'static str {
    env!("CARGO_BIN_EXE_harn")
}

fn run(cwd: &Path, args: &[&str]) -> Output {
    Command::new(harn_bin())
        .current_dir(cwd)
        .args(args)
        .env("HARN_LLM_CALLS_DISABLED", "1")
        .output()
        .expect("spawn harn")
}

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn generated_tool_and_connector_project_typed_harness_contracts() {
    let temp = tempfile::tempdir().expect("tempdir");

    let tool = temp.path().join("typed-tool");
    let tool_arg = tool.to_string_lossy();
    assert_success(
        "tool scaffold",
        &run(
            temp.path(),
            &["tool", "new", "typed-tool", "--dir", tool_arg.as_ref()],
        ),
    );
    let tool_main = fs::read_to_string(tool.join("main.harn")).expect("generated tool main");
    let tool_test =
        fs::read_to_string(tool.join("tests/test_tool.harn")).expect("generated tool test");
    for (path, source) in [
        ("main.harn", tool_main),
        ("tests/test_tool.harn", tool_test),
    ] {
        assert!(
            source.contains("harness: Harness"),
            "{path} must accept the typed Harness"
        );
        assert!(
            source.contains("agent_dispatch_tool_call(harness.tools,"),
            "{path} must project the HarnessTools sub-handle"
        );
    }
    assert_success("generated tool check", &run(&tool, &["check", "."]));

    assert_success(
        "connector scaffold",
        &run(temp.path(), &["new", "connector", "typed-connector"]),
    );
    let connector = temp.path().join("typed-connector");
    let connector_source = fs::read_to_string(connector.join("connectors/echo.harn"))
        .expect("generated connector module");
    assert!(
        connector_source.contains("pub fn normalize_inbound(_harness: Harness, raw)"),
        "connector runtime exports must project the typed leading Harness"
    );
    assert_success(
        "generated connector check",
        &run(&connector, &["check", "."]),
    );
}
