#![cfg(unix)]

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use harn_vm::event_log::{EventLog, SqliteEventLog, Topic};
use tempfile::TempDir;

// Since O-02 (#179/#215) landed, the orchestrator binds a real HTTP listener
// and prints a ready line. This test predates that work; the startup needle is
// updated to the post-#215 ready message so it keeps serving its real purpose
// (waiting for startup to complete before asserting dedupe behaviour).
const STARTUP_NEEDLE: &str = "HTTP listener ready on";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn copy_dir_recursive(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_dir_recursive(&source_path, &destination_path);
        } else {
            fs::copy(&source_path, &destination_path).unwrap();
        }
    }
}

fn seed_fixture(temp: &TempDir) {
    let fixture = repo_root().join("conformance/fixtures/triggers/inbox_dedupe_restart");
    copy_dir_recursive(&fixture, temp.path());
}

fn spawn_orchestrator(
    temp: &TempDir,
    extra_env: &[(&str, &str)],
) -> (Child, Receiver<String>, thread::JoinHandle<String>) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_harn"));
    command
        .current_dir(temp.path())
        .arg("orchestrator")
        .arg("serve")
        .arg("--config")
        .arg("harn.toml")
        .arg("--state-dir")
        .arg("./state")
        .arg("--role")
        .arg("single-tenant")
        .stderr(Stdio::piped())
        .stdout(Stdio::null());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command.spawn().unwrap();

    let stderr = child.stderr.take().expect("stderr pipe");
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut collected = String::new();
        for line in BufReader::new(stderr).lines() {
            let line = line.expect("stderr line");
            collected.push_str(&line);
            collected.push('\n');
            let _ = tx.send(line);
        }
        collected
    });

    (child, rx, handle)
}

fn wait_for_log_line(child: &mut Child, rx: &Receiver<String>, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) if line.contains(needle) => return,
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(status) = child.try_wait().unwrap() {
                    panic!("process exited before '{needle}' appeared: {status}");
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("stderr stream closed before '{needle}' appeared");
            }
        }
    }
    panic!("timed out waiting for '{needle}'");
}

fn wait_for_exit_code(child: &mut Child, expected: i32) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(
                status.code(),
                Some(expected),
                "unexpected exit status: {status}"
            );
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for orchestrator exit");
}

fn send_sigterm(child: &Child) {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(status.success(), "kill exited with {status}");
}

fn wait_for_successful_exit(child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "child exited unsuccessfully: {status}");
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for orchestrator exit");
}

async fn read_tick_dedupe_counts(temp: &TempDir) -> HashMap<String, usize> {
    let log = SqliteEventLog::open(temp.path().join("state/events.sqlite"), 32).unwrap();
    let topic = Topic::new("connectors.cron.tick").unwrap();
    let events = log.read_range(&topic, None, usize::MAX).await.unwrap();
    let mut counts = HashMap::new();
    for (_, event) in events {
        let dedupe_key = event
            .payload
            .get("dedupe_key")
            .and_then(|value| value.as_str())
            .expect("tick event dedupe key")
            .to_string();
        *counts.entry(dedupe_key).or_insert(0) += 1;
    }
    counts
}

#[tokio::test(flavor = "current_thread")]
async fn restart_after_emit_does_not_duplicate_cron_dispatch() {
    let temp = TempDir::new().unwrap();
    seed_fixture(&temp);

    let (mut crashing_child, crashing_rx, crashing_handle) =
        spawn_orchestrator(&temp, &[("HARN_TEST_CRON_FAIL_AFTER_EMIT", "1")]);
    wait_for_log_line(&mut crashing_child, &crashing_rx, STARTUP_NEEDLE);
    wait_for_exit_code(&mut crashing_child, 86);
    let crashing_stderr = crashing_handle.join().expect("stderr collector thread");
    assert!(crashing_stderr.contains("registered connectors (1): cron"));

    let counts_after_crash = read_tick_dedupe_counts(&temp).await;
    assert_eq!(counts_after_crash.len(), 1);
    let first_key = counts_after_crash
        .keys()
        .next()
        .expect("first crashed tick")
        .clone();

    let (mut second_child, second_rx, second_handle) = spawn_orchestrator(&temp, &[]);
    wait_for_log_line(&mut second_child, &second_rx, STARTUP_NEEDLE);
    thread::sleep(Duration::from_secs(3));
    send_sigterm(&second_child);
    wait_for_successful_exit(&mut second_child);
    let second_stderr = second_handle.join().expect("stderr collector thread");
    assert!(second_stderr.contains("registered connectors (1): cron"));

    let counts = read_tick_dedupe_counts(&temp).await;
    assert!(
        counts.len() >= 2,
        "expected restarted orchestrator to observe at least one later tick: {counts:?}"
    );
    assert_eq!(counts.get(&first_key), Some(&1));
    assert!(
        counts.values().all(|count| *count == 1),
        "saw duplicate cron dispatches after restart: {counts:?}"
    );
}
