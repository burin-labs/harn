//! Regression tests for `std/agent/host_tools` `run_command` argv handling.
//!
//! A cheap model driving the host `run_command` tool routinely sends the
//! command vector under the `command` field instead of `argv` (the tool
//! description says "pass argv as an array of strings" while the schema
//! declares `argv` as the array), e.g. `run({command: ["bash", "-lc", "ls"]})`.
//! Before the fix that threw "run_command: argv must be a non-empty list of
//! strings"; the tool only worked when the array was under `argv` or when a
//! shell string was passed under `command`.
//!
//! These tests spawn the real binary and drive the public `run_command`
//! handler. They use a `command_policy` that returns a string verdict, which
//! blocks the request BEFORE it executes — so the test is hermetic (no real
//! process spawn, no workspace/sandbox dependency) yet still asserts the
//! shaped request the tool would have run.

use std::process::Command;

const PROCESS_FIXTURE_LEVEL: &str = "HARN_TEST_PARALLEL_HOST_COMMAND_LEVEL";
const PROCESS_FIXTURE_RECEIPT: &str = "HARN_TEST_PARALLEL_HOST_COMMAND_RECEIPT";
const PROCESS_FIXTURE_FINISHED: &str = "HARN_TEST_PARALLEL_HOST_COMMAND_FINISHED";
const PROCESS_FIXTURE_TEST: &str =
    "agent_run_command_argv_coercion::parallel_host_command_process_fixture";

fn binary_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_harn"))
}

fn write_script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write script");
    path
}

fn run_script(body: &str) -> String {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let script = write_script(tmp.path(), "script.harn", body);
    let output = Command::new(binary_path())
        .arg("run")
        .arg(&script)
        .output()
        .expect("spawn harn run");
    assert!(
        output.status.success(),
        "harn run failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run_script_no_sandbox(body: &str) -> String {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let script = write_script(tmp.path(), "script.harn", body);
    let output = Command::new(binary_path())
        .args(["run", "--no-sandbox"])
        .arg(&script)
        .output()
        .expect("spawn harn run");
    assert!(
        output.status.success(),
        "harn run failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    format!(
        "{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

#[cfg(unix)]
fn process_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(windows)]
fn process_alive(pid: i32) -> bool {
    let output = Command::new("tasklist.exe")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()
        .expect("query Windows process");
    String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
}

/// Portable child/grandchild fixture for the real host-command cancellation
/// regression. The outer fixture waits for its child; the child writes a
/// forbidden marker only after ten seconds. Harn must terminate both levels.
#[test]
fn parallel_host_command_process_fixture() {
    let Some(level) = std::env::var_os(PROCESS_FIXTURE_LEVEL) else {
        return;
    };
    let level = level.to_string_lossy();
    let finished = std::env::var(PROCESS_FIXTURE_FINISHED).expect("fixture finished path");
    if level == "grandchild" {
        std::thread::park();
        std::fs::write(finished, "unexpected").expect("write forbidden completion marker");
        return;
    }
    assert_eq!(level, "root", "unknown process fixture level");
    let receipt = std::env::var(PROCESS_FIXTURE_RECEIPT).expect("fixture receipt path");
    let mut grandchild = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", PROCESS_FIXTURE_TEST, "--nocapture"])
        .env(PROCESS_FIXTURE_LEVEL, "grandchild")
        .env(PROCESS_FIXTURE_FINISHED, finished)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn process fixture grandchild");
    std::fs::write(
        receipt,
        format!("{} {}\n", std::process::id(), grandchild.id()),
    )
    .expect("write process fixture PID receipt");
    let _ = grandchild.wait();
}

/// A list passed under `command` is coerced to argv mode and produces the
/// IDENTICAL shaped request as passing the same list under `argv`.
#[test]
fn list_under_command_is_coerced_to_argv() {
    let body = r#"
import { agent_command_tools } from "std/agent/host_tools"

// Returning a string from command_policy blocks the request before it runs,
// while exposing the shaped request so we can assert its mode/argv.
fn capture(request: dict, args: unknown) {
  return "MODE=" + to_string(request?.mode) + " ARGV=" + json_stringify(request?.argv)
}

pipeline main(harness: Harness) {
  const opts = {root: "/workspace", command_policy: capture, output_format: "value"}
  const h = tool_find(agent_command_tools(harness, tool_registry(), opts), "run_command").handler
  const from_command = h({command: ["bash", "-lc", "ls -1"]})
  const from_argv = h({argv: ["bash", "-lc", "ls -1"]})
  harness.stdio.println("COMMAND_REASON=" + from_command.reason)
  harness.stdio.println("ARGV_REASON=" + from_argv.reason)
  harness.stdio.println("EQUAL=" + to_string(from_command.reason == from_argv.reason))
}

"#;
    let stdout = run_script(body);
    assert!(
        stdout.contains(r#"COMMAND_REASON=MODE=argv ARGV=["bash","-lc","ls -1"]"#),
        "list under `command` should shape into argv mode; got:\n{stdout}"
    );
    assert!(
        stdout.contains("EQUAL=true"),
        "list under `command` must equal the same list under `argv`; got:\n{stdout}"
    );
}

/// A real synchronous host command must not starve a fail-fast sibling. The
/// slow branch starts a child whose grandchild sleeps for ten seconds; the
/// other branch fails immediately. Cancellation must interrupt the process
/// group and drop the slow VM before it writes its post-command marker.
#[test]
fn parallel_host_command_cancellation_interrupts_child_group() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path().to_string_lossy().replace('\\', "/");
    let marker = format!("{root}/slow-branch-survived");
    let pid_receipt = format!("{root}/slow-pids");
    let child_finished = format!("{root}/child-finished");
    let fixture_binary = std::env::current_exe()
        .expect("test binary")
        .to_string_lossy()
        .replace('\\', "/");
    let body = format!(
        r#"
import {{ agent_command_tools }} from "std/agent/host_tools"

fn allow(request: dict, args: unknown) {{ return true }}

pipeline main(harness: Harness) {{
  const root = "{root}"
  const marker = "{marker}"
  const pid_receipt = "{pid_receipt}"
  const started_ms = harness.clock.monotonic_ms()
  const opts = {{
    root: root,
    allow_shell: false,
    command_policy: allow,
    output_format: "value",
  }}
  const run = tool_find(agent_command_tools(harness, tool_registry(), opts), "run_command").handler
  try {{
    parallel each ["slow", "fail"] with {{max_concurrent: 2}} {{ item ->
      if item == "fail" {{
        while !harness.fs.exists(pid_receipt) {{
          harness.runtime.yield_now()
        }}
        throw "sibling failure"
      }}
      run({{
        argv: ["{fixture_binary}", "--exact", "{PROCESS_FIXTURE_TEST}", "--nocapture"],
        env: {{
          "{PROCESS_FIXTURE_LEVEL}": "root",
          "{PROCESS_FIXTURE_RECEIPT}": pid_receipt,
          "{PROCESS_FIXTURE_FINISHED}": "{child_finished}",
        }},
      }})
      harness.fs.write_text(marker, "unexpected")
      return item
    }}
  }} catch (error) {{
    harness.stdio.println("ERROR=" + to_string(error))
  }}
  harness.stdio.println("MARKER=" + to_string(harness.fs.exists(marker)))
  harness.stdio.println("CANCEL_ELAPSED=" + to_string(harness.clock.monotonic_ms() - started_ms))
}}
"#,
    );
    let stdout = run_script_no_sandbox(&body);
    assert!(
        stdout.contains("ERROR=sibling failure"),
        "missing sibling error: {stdout}"
    );
    assert!(
        stdout.contains("MARKER=false"),
        "slow branch survived cancellation: {stdout}"
    );
    let cancel_elapsed = stdout
        .lines()
        .find_map(|line| line.strip_prefix("CANCEL_ELAPSED="))
        .and_then(|value| value.parse::<u64>().ok())
        .expect("numeric cancellation latency");
    assert!(
        cancel_elapsed < 5_000,
        "parallel cancellation exceeded five seconds: {cancel_elapsed}ms\n{stdout}"
    );
    assert!(
        !std::path::Path::new(&child_finished).exists(),
        "sleeping child completed naturally instead of being cancelled"
    );
    let pids = std::fs::read_to_string(&pid_receipt).expect("slow branch PID receipt");
    let pids = pids
        .split_whitespace()
        .map(|value| value.parse::<i32>().expect("numeric process id"))
        .collect::<Vec<_>>();
    assert_eq!(
        pids.len(),
        2,
        "expected shell and grandchild PIDs: {pids:?}"
    );
    for _ in 0..10_000 {
        if pids.iter().all(|pid| !process_alive(*pid)) {
            break;
        }
        std::thread::yield_now();
    }
    for pid in &pids {
        assert!(!process_alive(*pid), "cancelled process {pid} survived");
    }
}

/// A list passed under `argv` keeps working unchanged (no regression).
#[test]
fn list_under_argv_still_works() {
    let body = r#"
import { agent_command_tools } from "std/agent/host_tools"

fn capture(request: dict, args: unknown) {
  return "MODE=" + to_string(request?.mode)
}

pipeline main(harness: Harness) {
  const opts = {root: "/workspace", command_policy: capture, output_format: "value"}
  const h = tool_find(agent_command_tools(harness, tool_registry(), opts), "run_command").handler
  const r = h({argv: ["echo", "hi"]})
  harness.stdio.println("REASON=" + r.reason)
}
"#;
    let stdout = run_script(body);
    assert!(
        stdout.contains("REASON=MODE=argv"),
        "argv-mode must still shape correctly; got:\n{stdout}"
    );
}

/// The hostlib command request carries the configured command root as an
/// internal sandbox scope so hardened subprocesses can use temp workspaces
/// outside the ambient agent session root without widening model-visible tools.
#[test]
fn command_request_carries_root_as_internal_sandbox_scope() {
    let body = r#"
import { agent_command_tools } from "std/agent/host_tools"

fn capture(request: dict, args: unknown) {
  return "CWD=" + to_string(request?.cwd) + " SANDBOX=" + json_stringify(request?.sandbox?.workspace_roots)
}

pipeline main(harness: Harness) {
  const opts = {root: "/workspace", command_policy: capture, output_format: "value"}
  const h = tool_find(agent_command_tools(harness, tool_registry(), opts), "run_command").handler
  const r = h({argv: ["echo", "hi"]})
  harness.stdio.println("REASON=" + r.reason)
}
"#;
    let stdout = run_script(body);
    assert!(
        stdout.contains(r#"REASON=CWD=/workspace SANDBOX=["/workspace"]"#),
        "run_command should carry the configured root as an internal sandbox scope; got:\n{stdout}"
    );
}

/// A command policy can require host approval without throwing or running the command.
#[test]
fn command_policy_can_return_require_approval_verdict() {
    let body = r#"
import { agent_command_tools } from "std/agent/host_tools"

fn require_approval(request: dict, args: unknown) {
  return {require_approval: true, reason: "needs human approval", approval_id: "approval-123"}
}

pipeline main(harness: Harness) {
  const opts = {root: "/workspace", command_policy: require_approval, output_format: "value"}
  const h = tool_find(agent_command_tools(harness, tool_registry(), opts), "run_command").handler
  const r = h({argv: ["definitely-not-run-command-policy-sentinel"]})
  harness.stdio.println("STATUS=" + to_string(r.status))
  harness.stdio.println("REQUIRES=" + to_string(r.requires_approval))
  harness.stdio.println("REASON=" + r.reason)
  harness.stdio.println("APPROVAL_ID=" + r.approval_id)
  harness.stdio.println("MODE=" + to_string(r.request?.mode))
  harness.stdio.println("ARGV=" + json_stringify(r.request?.argv))
}
"#;
    let stdout = run_script(body);
    assert!(
        stdout.contains("STATUS=requires_approval"),
        "require_approval verdict should return a structured approval request; got:\n{stdout}"
    );
    assert!(
        stdout.contains("REQUIRES=true"),
        "approval result should carry requires_approval=true; got:\n{stdout}"
    );
    assert!(
        stdout.contains("REASON=needs human approval"),
        "approval result should preserve the policy reason; got:\n{stdout}"
    );
    assert!(
        stdout.contains("APPROVAL_ID=approval-123"),
        "approval result should preserve the optional approval id; got:\n{stdout}"
    );
    assert!(
        stdout.contains("MODE=argv")
            && stdout.contains(r#"ARGV=["definitely-not-run-command-policy-sentinel"]"#),
        "approval result should include the shaped command request; got:\n{stdout}"
    );
}

/// A non-empty `command` LIST routes to argv mode even when shell is disabled
/// (argv mode never needs a shell), instead of being rejected.
#[test]
fn list_under_command_works_without_shell() {
    let body = r#"
import { agent_command_tools } from "std/agent/host_tools"

fn capture(request: dict, args: unknown) {
  return "MODE=" + to_string(request?.mode)
}

pipeline main(harness: Harness) {
  const opts = {root: "/workspace", allow_shell: false, command_policy: capture, output_format: "value"}
  const h = tool_find(agent_command_tools(harness, tool_registry(), opts), "run_command").handler
  const r = h({command: ["bash", "-lc", "ls"]})
  harness.stdio.println("REASON=" + r.reason)
}
"#;
    let stdout = run_script(body);
    assert!(
        stdout.contains("REASON=MODE=argv"),
        "list under `command` must work without shell (argv mode); got:\n{stdout}"
    );
}

/// A non-string element in the command list yields the clear list-of-strings
/// error rather than silently coercing junk into argv.
#[test]
fn non_string_command_list_is_rejected() {
    let body = r#"
import { agent_command_tools } from "std/agent/host_tools"

pipeline main(harness: Harness) {
  const opts = {root: "/workspace", allow_shell: true, output_format: "value"}
  const h = tool_find(agent_command_tools(harness, tool_registry(), opts), "run_command").handler
  const r = try { h({command: ["bash", 7]}) }
  if is_ok(r) {
    harness.stdio.println("OUTCOME=unexpected-ok")
  } else {
    harness.stdio.println("OUTCOME=" + to_string(r))
  }
}
"#;
    let stdout = run_script(body);
    assert!(
        stdout.contains("argv must be a non-empty list of strings"),
        "non-string list element should be rejected clearly; got:\n{stdout}"
    );
}

/// A non-empty shell STRING under `command` with shell disabled gives an
/// actionable error (how to pass argv) instead of the misleading
/// "argv must be a non-empty list of strings".
#[test]
fn shell_string_with_shell_disabled_gives_actionable_error() {
    let body = r#"
import { agent_command_tools } from "std/agent/host_tools"

pipeline main(harness: Harness) {
  const opts = {root: "/workspace", allow_shell: false, output_format: "value"}
  const h = tool_find(agent_command_tools(harness, tool_registry(), opts), "run_command").handler
  const r = try { h({command: "echo hi"}) }
  if is_ok(r) {
    harness.stdio.println("OUTCOME=unexpected-ok")
  } else {
    harness.stdio.println("OUTCOME=" + to_string(r))
  }
}
"#;
    let stdout = run_script(body);
    assert!(
        stdout.contains("shell commands are disabled for this tool"),
        "shell-disabled string command should give an actionable error; got:\n{stdout}"
    );
    assert!(
        stdout.contains("pass argv as a list of strings"),
        "actionable error should tell the model to use argv; got:\n{stdout}"
    );
}
