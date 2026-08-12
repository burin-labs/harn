use crate::test_util::process::harn_e2e_command;

#[test]
#[cfg(target_os = "linux")]
fn timed_out_toolchain_probe_does_not_cancel_parent_run() {
    let source = r#"
import { command_run } from "std/command"

const probes = [
  ["uname", "-s"],
  ["git", "--version"],
  ["swift", "--version"],
  ["python3", "--version"],
  ["go", "version"],
]
for argv in probes {
  const attempted = try {
    command_run(harness.tools, argv, {timeout_ms: 10000})
  }
  harness.stdio.println("probe=" + to_string(argv[0]) + " result=" + to_string(attempted))
}
harness.stdio.println("parent-alive")
"#;
    let output = harn_e2e_command()
        .args(["run", "--no-sandbox", "-e", source])
        .output()
        .expect("spawn harn command probe regression");

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
