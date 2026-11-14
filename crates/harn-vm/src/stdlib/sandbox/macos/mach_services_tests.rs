//! A system service that transfers URLs for its caller runs outside the
//! caller's profile, so a confined command must not be able to reach one.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::mpsc;

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

/// Serve `listener` on its own thread. Each request's path is reported before
/// it gets a complete response, so a transfer that finished has always been
/// reported by the time its probe exits, and none is retried later.
fn serve(listener: TcpListener) -> mpsc::Receiver<String> {
    let (paths, received) = mpsc::channel();
    crate::runtime_stack::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut line = String::new();
            let _ = BufReader::new(&stream).read_line(&mut line);
            if let Some(path) = line.split_whitespace().nth(1) {
                let _ = paths.send(path.to_string());
            }
            let _ = (&stream)
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    received
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
    let requests = serve(listener);

    // Positive control: unconfined, the system service fetches the URL.
    let _ = Command::new(&probe).arg(url("unconfined")).output();
    if !requests.try_iter().any(|path| path == "/unconfined") {
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
    let reached: Vec<String> = requests.try_iter().collect();
    assert!(
        !reached.contains(&"/confined".to_string()),
        "a confined command reached a URL through a system service: {reached:?}"
    );
}
