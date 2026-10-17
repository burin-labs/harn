//! harn#9655: a redirect to a standard I/O device names no workspace file.
//! `2>/dev/null` made every command outside_workspace, including a host's own
//! catalog check against a file inside the workspace.

use super::*;

/// The host's Python static check, verbatim, against a workspace file.
const RUFF_CHECK: &str = "ruff check --isolated --select=F,E9 --output-format=json --exit-zero '/tmp/work/hello.py' 2>/dev/null";

#[test]
fn a_redirect_to_a_standard_stream_is_not_outside_the_workspace() {
    for command in [
        RUFF_CHECK,
        "echo hi 2>/dev/null",
        "echo hi >/dev/null 2>&1",
        "echo hi > /dev/stderr",
        "cat /dev/stdin > /tmp/work/out.txt",
        "echo hi 2>/dev/fd/2",
    ] {
        assert!(
            !is_outside_workspace(command),
            "a standard I/O device is not a workspace path: {command}"
        );
    }
    let scan = command_risk_scan_json(&shell_ctx(RUFF_CHECK), None);
    assert_eq!(scan["action"], "allow", "{scan}");
}

/// The exemption is the named devices, not `/dev`, and not the command.
#[test]
fn other_absolute_paths_beside_a_discarded_stream_are_still_outside() {
    for command in [
        "ruff check '/etc/hello.py' 2>/dev/null",
        "echo hi > /etc/harn-output 2>/dev/null",
        "echo hi > /dev/sda",
        "echo hi > /dev/fd/",
        "cat /dev/nullx",
    ] {
        assert!(
            is_outside_workspace(command),
            "an absolute path outside the roots must still be flagged: {command}"
        );
    }
}

/// A POSIX stream spelling is not a native Windows device. In native argv or
/// PowerShell it still denotes a filesystem operand outside these roots.
#[cfg(windows)]
#[test]
fn native_windows_paths_do_not_inherit_posix_stream_exemptions() {
    for context in [
        ctx(&["cat", "/dev/null"]),
        serde_json::json!({
            "request": {
                "mode": "shell",
                "command": "Write-Output hi > /dev/null",
                "cwd": "/tmp/work",
                "shell": { "id": "pwsh", "platform": "windows" },
            },
            "workspace_roots": ["/tmp/work"],
        }),
    ] {
        let scan = command_risk_scan_json(&context, None);
        assert!(
            labels(&scan).contains(&"outside_workspace".to_string()),
            "native Windows filesystem operands remain scoped: {scan}"
        );
    }
}
