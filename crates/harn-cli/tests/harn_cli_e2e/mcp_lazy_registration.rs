#![cfg(unix)]

use crate::test_util::process::harn_e2e_command;
use std::fs;
use tempfile::TempDir;

#[test]
fn help_and_version_leave_lazy_mcp_dormant_until_activation() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("child-started");
    let args = serde_json::json!([
        "-c",
        "printf started > \"$1\"",
        "fixture",
        marker.to_str().unwrap()
    ]);
    fs::write(
        temp.path().join("harn.toml"),
        format!(
            r#"
[package]
name = "lazy-help-fixture"
version = "0.1.0"
[[mcp]]
name = "lazy-help"
command = "sh"
args = {args}
lazy = true
[mcp.auth]
mode = "static"
secret_id = "invalid-secret-id"
"#
        ),
    )
    .unwrap();
    fs::write(
        temp.path().join("main.harn"),
        r#"
fn main(harness: Harness) {
  if argv.contains("--help") || argv.contains("--version") {
    harness.stdio.println("help-or-version-reached")
    return
  }
  try {
    harness.tools.mcp_ensure_active("lazy-help")
  } catch (error) {
    harness.stdio.println("activation-attempt-reached")
  }
}
"#,
    )
    .unwrap();
    for argument in ["--help", "--version", "--activate"] {
        let output = harn_e2e_command()
            .current_dir(temp.path())
            .args(["run", "--no-sandbox", "main.harn", "--", argument])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{argument}: {stdout}\n{stderr}");
        if argument == "--activate" {
            assert!(stdout.contains("activation-attempt-reached"), "{stdout}");
            assert_eq!(fs::read_to_string(&marker).unwrap(), "started");
            assert!(stderr.contains("failed to load auth"), "{stderr}");
        } else {
            assert!(stdout.contains("help-or-version-reached"), "{stdout}");
            assert!(
                !stderr.contains("mcp:"),
                "unexpected MCP preparation: {stderr}"
            );
            assert!(!marker.exists(), "help/version started a lazy child");
        }
    }
}
