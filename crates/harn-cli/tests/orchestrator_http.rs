#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use hmac::{Hmac, Mac};
use rcgen::generate_simple_self_signed;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE, ORIGIN};
use reqwest::Certificate;
use reqwest::StatusCode;
use sha2_10::Sha256;
use tempfile::TempDir;
use time::OffsetDateTime;

const STARTUP_PREFIX: &str = "[harn] HTTP listener ready on ";
const SHUTDOWN_NEEDLE: &str = "graceful shutdown complete";

type HmacSha256 = Hmac<Sha256>;

fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn write_bytes(dir: &Path, relative: &str, bytes: &[u8]) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, bytes).unwrap();
}

fn base_manifest(orchestrator_block: Option<&str>) -> String {
    let mut manifest = r#"
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
"#
    .to_string();
    if let Some(block) = orchestrator_block {
        manifest.push('\n');
        manifest.push_str(block);
        manifest.push('\n');
    }
    manifest
}

fn handler_module() -> &'static str {
    r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) {
  log(event.kind)
}
"#
}

fn a2a_manifest(orchestrator_block: Option<&str>) -> String {
    let mut manifest = r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "incoming-review-task"
kind = "a2a-push"
provider = "a2a-push"
path = "/a2a/review"
match = { events = ["a2a.task.received"] }
handler = "handlers::on_task"
"#
    .to_string();
    if let Some(block) = orchestrator_block {
        manifest.push('\n');
        manifest.push_str(block);
        manifest.push('\n');
    }
    manifest
}

fn a2a_handler_module() -> &'static str {
    r#"
import "std/triggers"

pub fn on_task(event: TriggerEvent) {
  log(event.kind)
}
"#
}

fn spawn_orchestrator(
    temp: &TempDir,
    extra_args: &[&str],
    envs: &[(&str, &str)],
) -> OrchestratorProcess {
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
        .arg("--bind")
        .arg("127.0.0.1:0")
        .stderr(Stdio::piped())
        .stdout(Stdio::null());
    for arg in extra_args {
        command.arg(arg);
    }
    for (key, value) in envs {
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

    OrchestratorProcess {
        child,
        rx,
        handle: Some(handle),
    }
}

struct OrchestratorProcess {
    child: Child,
    rx: Receiver<String>,
    handle: Option<thread::JoinHandle<String>>,
}

impl OrchestratorProcess {
    fn wait_for_listener_url(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(line) if line.contains(STARTUP_PREFIX) => {
                    return line
                        .split(STARTUP_PREFIX)
                        .nth(1)
                        .expect("startup line has URL")
                        .trim()
                        .to_string();
                }
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(status) = self.child.try_wait().unwrap() {
                        let stderr = self.join_stderr();
                        panic!(
                            "process exited before listener became ready: {status}\nstderr={stderr}"
                        );
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let stderr = self.join_stderr();
                    panic!("stderr stream closed before listener became ready\nstderr={stderr}");
                }
            }
        }
        let stderr = self.join_stderr();
        panic!("timed out waiting for listener startup\nstderr={stderr}");
    }

    fn join_stderr(&mut self) -> String {
        self.handle
            .take()
            .expect("stderr collector thread")
            .join()
            .expect("stderr collector result")
    }
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
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "child exited unsuccessfully: {status}");
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for orchestrator exit");
}

fn github_signature(secret: &str, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!("sha256={encoded}")
}

fn github_headers(secret: &str, body: &[u8], origin: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert("X-GitHub-Event", HeaderValue::from_static("issues"));
    headers.insert(
        "X-GitHub-Delivery",
        HeaderValue::from_static("delivery-123"),
    );
    headers.insert(
        "X-Hub-Signature-256",
        HeaderValue::from_str(&github_signature(secret, body)).unwrap(),
    );
    if let Some(origin) = origin {
        headers.insert(ORIGIN, HeaderValue::from_str(origin).unwrap());
    }
    headers
}

fn state_snapshot(temp: &TempDir) -> String {
    fs::read_to_string(temp.path().join("state/orchestrator-state.json")).unwrap()
}

async fn assert_status(response: reqwest::Response, expected: StatusCode) {
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, expected, "status={status} body={body}");
}

fn json_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers
}

#[tokio::test(flavor = "multi_thread")]
async fn github_webhook_delivery_is_accepted_and_persisted() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "integration-test-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let health = reqwest::get(format!("{base_url}/health")).await.unwrap();
    assert_status(health, StatusCode::OK).await;

    let body = br#"{"action":"opened","issue":{"number":1}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, body, None))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    send_sigterm(&process.child);
    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot = state_snapshot(&temp);
    assert!(
        snapshot.contains("\"status\": \"stopped\""),
        "snapshot={snapshot}"
    );
    assert!(snapshot.contains("\"received\": 1"), "snapshot={snapshot}");
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a2a_push_route_requires_bearer_or_valid_hmac() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &a2a_manifest(None));
    write_file(temp.path(), "lib.harn", a2a_handler_module());

    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_ORCHESTRATOR_API_KEYS", "test-key-1,test-key-2"),
        ("HARN_ORCHESTRATOR_HMAC_SECRET", "shared-secret"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let client = reqwest::Client::new();
    let body = br#"{"kind":"a2a.task.received","task":{"id":"task-123"}}"#;

    let response = client
        .post(format!("{base_url}/a2a/review"))
        .headers(json_headers())
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::UNAUTHORIZED).await;

    let timestamp = OffsetDateTime::now_utc().unix_timestamp();
    let mut wrong_hmac_headers = json_headers();
    wrong_hmac_headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!(
            "HMAC-SHA256 timestamp={timestamp},signature=AAAAAAAAAAAAAAAAAAAAAA=="
        ))
        .unwrap(),
    );
    let response = client
        .post(format!("{base_url}/a2a/review"))
        .headers(wrong_hmac_headers)
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::UNAUTHORIZED).await;

    let mut bearer_headers = json_headers();
    bearer_headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer test-key-2"));
    let response = client
        .post(format!("{base_url}/a2a/review"))
        .headers(bearer_headers)
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    send_sigterm(&process.child);
    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot = state_snapshot(&temp);
    assert!(snapshot.contains("\"received\": 3"), "snapshot={snapshot}");
    assert!(snapshot.contains("\"failed\": 2"), "snapshot={snapshot}");
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tls_listener_serves_https_with_supplied_cert_and_key() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let cert = generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .unwrap();
    let cert_pem = cert.cert.pem();
    let key_pem = cert.key_pair.serialize_pem();
    write_bytes(temp.path(), "tls/cert.pem", cert_pem.as_bytes());
    write_bytes(temp.path(), "tls/key.pem", key_pem.as_bytes());

    let secret = "tls-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
    ];
    let args = ["--cert", "tls/cert.pem", "--key", "tls/key.pem"];
    let mut process = spawn_orchestrator(&temp, &args, &envs);
    let base_url = process.wait_for_listener_url();
    assert!(base_url.starts_with("https://"), "{base_url}");

    let body = br#"{"action":"opened","issue":{"number":2}}"#;
    let response = reqwest::Client::builder()
        .add_root_certificate(Certificate::from_pem(cert_pem.as_bytes()).unwrap())
        .build()
        .unwrap()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, body, None))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::OK).await;

    send_sigterm(&process.child);
    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn disallowed_origin_is_rejected() {
    let temp = TempDir::new().unwrap();
    write_file(
        temp.path(),
        "harn.toml",
        &base_manifest(Some(
            r#"[orchestrator]
allowed_origins = ["https://allowed.example"]"#,
        )),
    );
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "origin-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = br#"{"action":"opened","issue":{"number":3}}"#;
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(
            secret,
            body,
            Some("https://blocked.example"),
        ))
        .body(body.to_vec())
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::FORBIDDEN).await;

    send_sigterm(&process.child);
    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_request_body_is_rejected() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "body-limit-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = vec![b'a'; (10 * 1024 * 1024) + 1];
    let response = reqwest::Client::new()
        .post(format!("{base_url}/triggers/github-new-issue"))
        .headers(github_headers(secret, &body, None))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_status(response, StatusCode::PAYLOAD_TOO_LARGE).await;

    send_sigterm(&process.child);
    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn graceful_shutdown_waits_for_in_flight_request() {
    let temp = TempDir::new().unwrap();
    write_file(temp.path(), "harn.toml", &base_manifest(None));
    write_file(temp.path(), "lib.harn", handler_module());

    let secret = "shutdown-secret";
    let envs = [
        ("HARN_SECRET_PROVIDERS", "env"),
        ("HARN_SECRET_GITHUB_WEBHOOK_SECRET", secret),
        ("HARN_ORCHESTRATOR_TEST_REQUEST_DELAY_MS", "500"),
    ];
    let mut process = spawn_orchestrator(&temp, &[], &envs);
    let base_url = process.wait_for_listener_url();

    let body = br#"{"action":"opened","issue":{"number":4}}"#.to_vec();
    let request = tokio::spawn({
        let client = reqwest::Client::new();
        let url = format!("{base_url}/triggers/github-new-issue");
        let headers = github_headers(secret, &body, None);
        async move { client.post(url).headers(headers).body(body).send().await }
    });

    tokio::time::sleep(Duration::from_millis(100)).await;
    send_sigterm(&process.child);
    let response = request.await.unwrap().unwrap();
    assert_status(response, StatusCode::OK).await;

    wait_for_exit(&mut process.child);
    let stderr = process.join_stderr();
    assert!(stderr.contains(SHUTDOWN_NEEDLE), "stderr={stderr}");

    let snapshot = state_snapshot(&temp);
    assert!(
        snapshot.contains("\"dispatched\": 1"),
        "snapshot={snapshot}"
    );
    assert!(snapshot.contains("\"in_flight\": 0"), "snapshot={snapshot}");
}
