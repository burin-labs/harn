use std::process::Stdio;

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::test_util::process::{harn_e2e_binary, harn_e2e_command};

#[tokio::test]
async fn portal_announces_a_live_listener_on_stderr() {
    let temp = TempDir::new().unwrap();
    let mut child = tokio::process::Command::new(harn_e2e_binary())
        .current_dir(temp.path())
        .args(["portal", "--port", "0", "--open", "false"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("start portal");
    let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
    let line = harn_clock::test_support::within("portal readiness", stderr.next_line())
        .await
        .expect("read portal readiness")
        .expect("portal exited before readiness");
    let url = line
        .strip_prefix("[harn] Harn portal listening on ")
        .expect("readiness must use the shared stderr prefix");
    let response = harn_clock::test_support::within("portal HTTP response", reqwest::get(url))
        .await
        .expect("the announced listener must accept connections");
    assert!(response.status().is_success());
    child.kill().await.expect("stop portal");
    let output = child.wait_with_output().await.expect("reap portal");
    assert!(output.stdout.is_empty(), "portal status reached stdout");
}

#[test]
fn portal_bind_failure_does_not_claim_readiness() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let temp = TempDir::new().unwrap();
    let output = harn_e2e_command()
        .current_dir(temp.path())
        .args(["portal", "--port", &port, "--open", "false"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("[harn] error:"), "{stderr}");
    assert!(!stderr.contains("portal listening"), "{stderr}");
}
