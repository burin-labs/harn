use crate::test_util::process::harn_e2e_command;

#[test]
fn handled_missing_command_does_not_cancel_parent_run() {
    #[cfg(windows)]
    let successful_argv = r#"["cmd.exe", "/C", "exit", "0"]"#;
    #[cfg(not(windows))]
    let successful_argv = r#"["sh", "-c", "exit 0"]"#;
    let source = r#"
import { command_run } from "std/command"

const attempted = try {
  command_run(
    harness.tools,
    ["harn-command-that-definitely-does-not-exist-6573"],
    {timeout_ms: 10000},
  )
}
assert(is_err(attempted), "missing command must remain a catchable host error")

const after = command_run(harness.tools, __SUCCESSFUL_ARGV__, {timeout_ms: 10000})
assert((after?.exit_code ?? -1) == 0, "a later command must still execute")
harness.stdio.println("parent-alive")
"#
    .replace("__SUCCESSFUL_ARGV__", successful_argv);
    let output = harn_e2e_command()
        .args(["run", "--no-sandbox", "-e", &source])
        .output()
        .expect("spawn handled missing-command cancellation regression");

    assert!(
        output.status.success(),
        "status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("parent-alive"),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
