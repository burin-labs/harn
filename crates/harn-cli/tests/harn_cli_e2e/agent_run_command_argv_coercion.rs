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

/// A list passed under `command` is coerced to argv mode and produces the
/// IDENTICAL shaped request as passing the same list under `argv`.
#[test]
fn list_under_command_is_coerced_to_argv() {
    let body = r#"
import { agent_command_tools } from "std/agent/host_tools"

// Returning a string from command_policy blocks the request before it runs,
// while exposing the shaped request so we can assert its mode/argv.
fn capture(request, args) {
  return "MODE=" + to_string(request?.mode) + " ARGV=" + json_stringify(request?.argv)
}

pipeline main(_) {
  const opts = {root: "/workspace", command_policy: capture, output_format: "value"}
  const h = tool_find(agent_command_tools(tool_registry(), opts), "run_command").handler
  const from_command = h({command: ["bash", "-lc", "ls -1"]})
  const from_argv = h({argv: ["bash", "-lc", "ls -1"]})
  __io_println("COMMAND_REASON=" + from_command.reason)
  __io_println("ARGV_REASON=" + from_argv.reason)
  __io_println("EQUAL=" + to_string(from_command.reason == from_argv.reason))
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

/// A list passed under `argv` keeps working unchanged (no regression).
#[test]
fn list_under_argv_still_works() {
    let body = r#"
import { agent_command_tools } from "std/agent/host_tools"

fn capture(request, args) {
  return "MODE=" + to_string(request?.mode)
}

pipeline main(_) {
  const opts = {root: "/workspace", command_policy: capture, output_format: "value"}
  const h = tool_find(agent_command_tools(tool_registry(), opts), "run_command").handler
  const r = h({argv: ["echo", "hi"]})
  __io_println("REASON=" + r.reason)
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

fn capture(request, args) {
  return "CWD=" + to_string(request?.cwd) + " SANDBOX=" + json_stringify(request?.sandbox?.workspace_roots)
}

pipeline main(_) {
  const opts = {root: "/workspace", command_policy: capture, output_format: "value"}
  const h = tool_find(agent_command_tools(tool_registry(), opts), "run_command").handler
  const r = h({argv: ["echo", "hi"]})
  __io_println("REASON=" + r.reason)
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

fn require_approval(request, args) {
  return {require_approval: true, reason: "needs human approval", approval_id: "approval-123"}
}

pipeline main(_) {
  const opts = {root: "/workspace", command_policy: require_approval, output_format: "value"}
  const h = tool_find(agent_command_tools(tool_registry(), opts), "run_command").handler
  const r = h({argv: ["definitely-not-run-command-policy-sentinel"]})
  __io_println("STATUS=" + to_string(r.status))
  __io_println("REQUIRES=" + to_string(r.requires_approval))
  __io_println("REASON=" + r.reason)
  __io_println("APPROVAL_ID=" + r.approval_id)
  __io_println("MODE=" + to_string(r.request?.mode))
  __io_println("ARGV=" + json_stringify(r.request?.argv))
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

fn capture(request, args) {
  return "MODE=" + to_string(request?.mode)
}

pipeline main(_) {
  const opts = {root: "/workspace", allow_shell: false, command_policy: capture, output_format: "value"}
  const h = tool_find(agent_command_tools(tool_registry(), opts), "run_command").handler
  const r = h({command: ["bash", "-lc", "ls"]})
  __io_println("REASON=" + r.reason)
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

pipeline main(_) {
  const opts = {root: "/workspace", allow_shell: true, output_format: "value"}
  const h = tool_find(agent_command_tools(tool_registry(), opts), "run_command").handler
  const r = try { h({command: ["bash", 7]}) }
  if is_ok(r) {
    __io_println("OUTCOME=unexpected-ok")
  } else {
    __io_println("OUTCOME=" + to_string(r))
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

pipeline main(_) {
  const opts = {root: "/workspace", allow_shell: false, output_format: "value"}
  const h = tool_find(agent_command_tools(tool_registry(), opts), "run_command").handler
  const r = try { h({command: "echo hi"}) }
  if is_ok(r) {
    __io_println("OUTCOME=unexpected-ok")
  } else {
    __io_println("OUTCOME=" + to_string(r))
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
