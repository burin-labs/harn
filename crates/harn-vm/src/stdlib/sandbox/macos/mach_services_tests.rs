//! A system service that transfers URLs for its caller runs outside the
//! caller's profile, so a confined command must not be able to reach one.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;
use std::time::{Duration, Instant};

use super::tests::macos_policy_with_workspace_ops;
use super::*;

/// Downloads its one argument through a background URL session and exits when
/// the transfer ends either way.
const PROBE: &str = r#"import Foundation
final class Done: NSObject, URLSessionDownloadDelegate {
  func urlSession(_ s: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo l: URL) { exit(0) }
  func urlSession(_ s: URLSession, task: URLSessionTask, didCompleteWithError e: Error?) { exit(e == nil ? 0 : 2) }
}
let config = URLSessionConfiguration.background(withIdentifier: "dev.harn.sandbox-probe.\(getpid())")
let session = URLSession(configuration: config, delegate: Done(), delegateQueue: nil)
session.downloadTask(with: URL(string: CommandLine.arguments[1])!).resume()
DispatchQueue.main.asyncAfter(deadline: .now() + 15) { exit(3) }
dispatchMain()
"#;

/// The request paths `listener` received within `wait`. Each request gets a
/// complete response, so a finished transfer is not retried later.
fn requests(listener: &TcpListener, wait: Duration) -> Vec<String> {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + wait;
    let mut paths = Vec::new();
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                let mut reader = BufReader::new(&stream);
                let _ = reader.read_line(&mut line);
                let _ = (&stream).write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
                paths.extend(line.split_whitespace().nth(1).map(str::to_string));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    paths
}

#[test]
fn a_confined_command_cannot_start_a_background_transfer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let source = root.join("probe.swift");
    let probe = root.join("probe");
    std::fs::write(&source, PROBE).unwrap();
    let compiled = Command::new("/usr/bin/xcrun")
        .args(["swiftc", "-O", "-o"])
        .arg(&probe)
        .arg(&source)
        .output();
    if !compiled
        .as_ref()
        .is_ok_and(|output| output.status.success())
    {
        eprintln!("skipping: no Swift compiler to build the probe: {compiled:?}");
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = |path: &str| format!("http://127.0.0.1:{port}/{path}");

    // Positive control: unconfined, the system service fetches the URL.
    let _ = Command::new(&probe).arg(url("unconfined")).output();
    if !requests(&listener, Duration::from_secs(2)).contains(&"/unconfined".to_string()) {
        eprintln!("skipping: background transfers do not run on this host");
        return;
    }

    let mut policy = macos_policy_with_workspace_ops(&[]);
    policy.workspace_roots = vec![root.display().to_string()];
    let config = ProcessCommandConfig {
        cwd: Some(root),
        ..Default::default()
    };
    let result = Backend::run_to_output(
        &probe.display().to_string(),
        &[url("confined")],
        &config,
        &policy,
        SandboxProfile::Worktree,
    );
    if crate::sandbox_host::assert_unavailable_result(&result) {
        return;
    }
    let output = result.expect("the confined probe runs");
    assert_ne!(
        output.status.code(),
        Some(0),
        "the transfer must not complete"
    );
    let reached = requests(&listener, Duration::from_secs(3));
    assert!(
        !reached.contains(&"/confined".to_string()),
        "a confined command reached a URL through a system service: {reached:?}"
    );
}
