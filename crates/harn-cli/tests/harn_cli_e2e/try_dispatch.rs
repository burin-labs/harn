//! Dispatch coverage for `harn try`.

use std::process::Command;

#[test]
fn try_dispatch_surfaces_tool_format_override_warning() {
    let output = Command::new(env!("CARGO_BIN_EXE_harn"))
        .env("HARN_LLM_PROVIDER", "mock")
        .env("HARN_LLM_MODEL", "claude-opus-4-7")
        .arg("try")
        .arg("--tool-format")
        .arg("text")
        .arg("--override-reason")
        .arg("compare text trace")
        .arg("Say hello.")
        .output()
        .expect("spawn harn try");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    assert_eq!(
        output.status.code().unwrap_or(-1),
        0,
        "try failed\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stderr.contains(
            "warning: tool_format override: mock:claude-opus-4-7 requested text over recommended native"
        ),
        "stderr should surface the override warning; got:\n{stderr}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "stdout should still include the assistant response"
    );
}
