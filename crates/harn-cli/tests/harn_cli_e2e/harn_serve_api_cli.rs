//! Product-path regressions for the file-backed Agents API server.

use crate::test_util;

use std::fs;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};
use tempfile::TempDir;
use test_util::process::harn_e2e_command;

const PROCESS_READY_TIMEOUT: Duration = Duration::from_mins(1);
const TASK_TERMINAL_TIMEOUT: Duration = Duration::from_secs(30);
const TEST_API_KEY: &str = "fixture-api-key-6799";

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn write_privileged_project(temp: &TempDir) -> std::path::PathBuf {
    fs::create_dir(temp.path().join(".git")).expect("project boundary");
    fs::write(
        temp.path().join("harn.toml"),
        r#"
[package]
name = "serve-api-host-dispatch-fixture"

[check]
trusted_host_dispatch = true

[exports]
trigger_handlers = "trigger_handlers.harn"

[[triggers]]
id = "cron-handler"
kind = "cron"
provider = "cron"
schedule = "* * * * *"
match = { events = ["cron.tick"] }
handler = "trigger_handlers::on_tick"
"#,
    )
    .expect("manifest fixture");
    fs::write(
        temp.path().join("trigger_handlers.harn"),
        r#"
pub fn on_tick(_event) -> nil {
  const _ = host_call("runtime.pipeline_input", {})
  return nil
}
"#,
    )
    .expect("privileged module fixture");
    let pipeline = temp.path().join("main.harn");
    fs::write(
        &pipeline,
        r#"
pipeline main(harness: Harness) {
  harness.stdio.println("api-body-reached")
}
"#,
    )
    .expect("pipeline fixture");
    pipeline
}

fn wait_for_listener(child: &mut Child, rx: &Receiver<String>) -> String {
    test_util::stdio_jsonrpc::wait_for_child_log_suffix(
        child,
        rx,
        "Agents API server ready on ",
        PROCESS_READY_TIMEOUT,
        "Agents API server",
    )
}

fn assert_tree_omits(path: &Path, needle: &str) {
    for entry in fs::read_dir(path).expect("read fixture tree") {
        let entry = entry.expect("fixture entry");
        let path = entry.path();
        if path.is_dir() {
            assert_tree_omits(&path, needle);
        } else if let Ok(contents) = fs::read(&path) {
            assert!(
                !contents
                    .windows(needle.len())
                    .any(|window| window == needle.as_bytes()),
                "credential leaked to {}",
                path.display()
            );
        }
    }
}

#[ignore = "binary surface — runs in the slow E2E/smoke job"]
#[tokio::test]
async fn serve_api_honors_manifest_authority_on_a_submitted_task() {
    let temp = TempDir::new().expect("temp project");
    let pipeline = write_privileged_project(&temp);
    let child = harn_e2e_command()
        .current_dir(temp.path())
        .arg("serve")
        .arg("api")
        .arg("--bind")
        .arg("127.0.0.1:0")
        .arg("--api-key")
        .arg(TEST_API_KEY)
        .arg(&pipeline)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn harn serve api");
    let mut child = ChildGuard(child);
    let (rx, stderr) =
        test_util::stdio_jsonrpc::spawn_line_reader(child.0.stderr.take().expect("stderr pipe"));
    let url = wait_for_listener(&mut child.0, &rx);
    let client = reqwest::Client::new();

    let session: Value = client
        .post(format!("{url}/v1/sessions"))
        .bearer_auth(TEST_API_KEY)
        .json(&json!({"workspace_id": "local"}))
        .send()
        .await
        .expect("create session")
        .error_for_status()
        .expect("session status")
        .json()
        .await
        .expect("session JSON");
    let session_id = session["id"].as_str().expect("session id");

    let task: Value = client
        .post(format!("{url}/v1/sessions/{session_id}/tasks"))
        .bearer_auth(TEST_API_KEY)
        .json(&json!({
            "input": {
                "role": "user",
                "parts": [{
                    "type": "text",
                    "text": "exercise the manifest-declared API pipeline",
                    "visibility": "public"
                }]
            }
        }))
        .send()
        .await
        .expect("submit task")
        .error_for_status()
        .expect("task status")
        .json()
        .await
        .expect("task JSON");
    let task_id = task["id"].as_str().expect("task id");

    let response = client
        .get(format!("{url}/v1/tasks/{task_id}/stream"))
        .bearer_auth(TEST_API_KEY)
        .send()
        .await
        .expect("subscribe to task events")
        .error_for_status()
        .expect("task event stream status");
    let mut stream = response.bytes_stream();
    let (terminal, body_reached) = tokio::time::timeout(TASK_TERMINAL_TIMEOUT, async {
        let mut buffer = String::new();
        let mut body_reached = false;
        while let Some(chunk) = stream.next().await {
            buffer.push_str(&String::from_utf8_lossy(&chunk.expect("task event chunk")));
            body_reached |= buffer.contains("api-body-reached");
            for frame in buffer.split("\n\n") {
                if frame.contains("event:task.completed")
                    || frame.contains("event: task.completed")
                    || frame.contains("\"event\":\"task.completed\"")
                {
                    return ("completed", body_reached);
                }
                if frame.contains("event:task.failed")
                    || frame.contains("event: task.failed")
                    || frame.contains("\"event\":\"task.failed\"")
                {
                    return ("failed", body_reached);
                }
            }
        }
        ("closed", body_reached)
    })
    .await
    .expect("task reached a terminal event");
    assert_eq!(
        terminal, "completed",
        "API task must reach the pipeline body"
    );
    assert!(
        body_reached,
        "API event stream omitted pipeline body output"
    );

    drop(child);
    let stderr = stderr.join().expect("join stderr reader");
    assert!(
        !stderr.contains(TEST_API_KEY),
        "credential leaked to server log"
    );
    assert_tree_omits(temp.path(), TEST_API_KEY);
}
