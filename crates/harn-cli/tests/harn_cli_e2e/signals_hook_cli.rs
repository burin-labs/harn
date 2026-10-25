//! The `harn signals hook` contract with agent harnesses. Claude Code and Codex
//! read these outputs and exit codes directly, so a wrong field name or a
//! stray exit 2 silently drops a reminder or steers a session.

use std::io::Write;
use std::path::Path;
use std::process::{Output, Stdio};

use tempfile::TempDir;

use crate::test_util::process::harn_e2e_command;

fn harn(dir: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = harn_e2e_command()
        .args(["signals", "--dir"])
        .arg(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn harn");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().expect("wait for harn")
}

fn fire(dir: &Path, key: &str, severity: &str) {
    let output = harn(
        dir,
        &[
            "fire",
            "--key",
            key,
            "--summary",
            "CI is red",
            "--source-url",
            "https://example.test/run/1",
            "--cwd",
            "/work",
            "--severity",
            severity,
        ],
        "",
    );
    assert!(output.status.success(), "{output:?}");
}

fn hook(dir: &Path, format: &str, event: &str) -> Output {
    harn(
        dir,
        &["hook", "--format", format, "--event", event],
        r#"{"session_id":"s1","cwd":"/work/app","hook_event_name":"x"}"#,
    )
}

fn json(output: &Output) -> serde_json::Value {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    serde_json::from_slice(&output.stdout).expect("hook stdout is one JSON object")
}

#[test]
fn post_tool_use_adds_context_once_and_the_next_hook_acks_it() {
    let dir = TempDir::new().unwrap();
    fire(dir.path(), "ci:app", "warning");
    let value = json(&hook(dir.path(), "claude-code", "PostToolUse"));
    let output = &value["hookSpecificOutput"];
    assert_eq!(output["hookEventName"], "PostToolUse");
    let text = output["additionalContext"].as_str().unwrap();
    assert!(
        text.contains("ci:app#1") && text.contains("CI is red"),
        "{text}"
    );
    assert!(text.chars().count() <= 10_000);

    let again = hook(dir.path(), "claude-code", "PostToolUse");
    assert_eq!(again.status.code(), Some(0));
    assert!(again.stdout.is_empty(), "a reminder was delivered twice");
    let list = harn(dir.path(), &["list"], "");
    let list = String::from_utf8_lossy(&list.stdout);
    assert!(list.contains("acked by s1"), "{list}");
}

#[test]
fn stop_keeps_non_urgent_reminders_for_the_next_prompt() {
    let dir = TempDir::new().unwrap();
    fire(dir.path(), "prs:app", "info");
    let stop = hook(dir.path(), "claude-code", "Stop");
    assert_eq!(stop.status.code(), Some(0));
    assert!(stop.stdout.is_empty(), "a non-urgent reminder ended a stop");
    let value = json(&hook(dir.path(), "claude-code", "UserPromptSubmit"));
    assert_eq!(
        value["hookSpecificOutput"]["hookEventName"],
        "UserPromptSubmit"
    );
}

#[test]
fn an_urgent_reminder_never_reaches_a_codex_session_as_a_user_prompt() {
    let dir = TempDir::new().unwrap();
    fire(dir.path(), "alert:claude", "urgent");
    let claude = json(&hook(dir.path(), "claude-code", "Stop"));
    assert_eq!(claude["hookSpecificOutput"]["hookEventName"], "Stop");

    // Codex turns a Stop `decision: block` reason into a user prompt, so its
    // Stop hook leases nothing and the alert waits for developer context.
    let codex_dir = TempDir::new().unwrap();
    fire(codex_dir.path(), "alert:codex", "urgent");
    let stop = json(&hook(codex_dir.path(), "codex", "Stop"));
    assert_eq!(stop, serde_json::json!({}));
    let next = json(&hook(codex_dir.path(), "codex", "UserPromptSubmit"));
    let text = next["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(text.contains("alert:codex#1"), "{text}");
}

#[test]
fn a_full_inbox_is_delivered_a_few_reminders_per_hook() {
    let dir = TempDir::new().unwrap();
    for index in 0..5 {
        fire(dir.path(), &format!("ci:app{index}"), "warning");
    }
    let first = json(&hook(dir.path(), "claude-code", "PostToolUse"));
    let text = first["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert_eq!(text.matches("<signal-data ").count(), 3, "{text}");
    assert!(text.contains("2 more signals"), "{text}");
    let second = json(&hook(dir.path(), "claude-code", "PostToolUse"));
    let text = second["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert_eq!(text.matches("<signal-data ").count(), 2, "{text}");
}

#[test]
fn a_second_rewake_watcher_for_one_session_exits_at_once() {
    let dir = TempDir::new().unwrap();
    let payload = r#"{"session_id":"s1","cwd":"/work/app"}"#;
    let mut first = harn_e2e_command()
        .args(["signals", "--dir"])
        .arg(dir.path())
        .args([
            "hook",
            "--format",
            "claude-code",
            "--event",
            "Stop",
            "--rewake",
            "--max-wait",
            "20s",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    first
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    // Wait for the first watcher to take its lock.
    let watchers = dir.path().join("watchers");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::fs::read_dir(&watchers).map_or(true, |mut d| d.next().is_none()) {
        assert!(
            std::time::Instant::now() < deadline,
            "watcher never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    let started = std::time::Instant::now();
    let second = harn(
        dir.path(),
        &[
            "hook",
            "--format",
            "claude-code",
            "--event",
            "Stop",
            "--rewake",
            "--max-wait",
            "20s",
        ],
        payload,
    );
    assert_eq!(second.status.code(), Some(0), "{second:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));

    // The first watcher still wakes the session when an urgent signal lands.
    fire(dir.path(), "alert:app", "urgent");
    let output = first.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("alert:app#1"));
}

#[test]
fn a_broken_hook_never_exits_two() {
    let dir = TempDir::new().unwrap();
    let output = harn(
        dir.path(),
        &["hook", "--format", "claude-code", "--event", "Stop"],
        "not json",
    );
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("harn signals hook"));
}
