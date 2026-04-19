#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

const STARTUP_NEEDLE: &str = "HTTP listener ready on";
const SHUTDOWN_NEEDLE: &str = "graceful shutdown complete";

fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn spawn_orchestrator(temp: &TempDir) -> (Child, Receiver<String>, thread::JoinHandle<String>) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_harn"))
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
        .stdout(Stdio::null())
        .spawn()
        .unwrap();

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

fn send_sigterm(child: &Child) {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(status.success(), "kill exited with {status}");
}

fn wait_for_exit(child: &mut Child) {
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

#[test]
fn orchestrator_serve_starts_and_shuts_down_cleanly() {
    let temp = TempDir::new().unwrap();
    write_file(
        temp.path(),
        "harn.toml",
        r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_issue"
secrets = { signing_secret = "github/webhook-secret" }
"#,
    );
    write_file(
        temp.path(),
        "lib.harn",
        r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) {
  log(event.kind)
}
"#,
    );

    let (mut child, rx, handle) = spawn_orchestrator(&temp);
    wait_for_log_line(&mut child, &rx, STARTUP_NEEDLE);
    send_sigterm(&child);
    wait_for_exit(&mut child);
    let stderr = handle.join().expect("stderr collector thread");

    assert!(stderr.contains("secret providers:"), "stderr={stderr}");
    assert!(
        stderr.contains("registered triggers (1):"),
        "stderr={stderr}"
    );
    assert!(
        stderr.contains("registered connectors (1):"),
        "stderr={stderr}"
    );
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot = temp.path().join("state/orchestrator-state.json");
    let snapshot_contents = fs::read_to_string(&snapshot).unwrap();
    assert!(snapshot_contents.contains("\"status\": \"stopped\""));
    assert!(snapshot_contents.contains("\"bind\": \"127.0.0.1:8080\""));
}
