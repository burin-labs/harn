#[cfg(target_os = "macos")]
mod macos {
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::process::Command;
    use std::thread;

    fn isolated_harn(root: &Path) -> Command {
        let mut command = Command::new(super::super::binary_path());
        command
            .current_dir(root)
            .env("HARN_STATE_DIR", root.join("state"))
            .env("HARN_RUN_DIR", root.join("runs"))
            .env("HARN_EVENT_LOG_DIR", root.join("events"))
            .env("HARN_CACHE_DIR", root.join("cache"));
        command
    }

    fn files_beneath(root: &Path) -> Vec<std::path::PathBuf> {
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(path) = pending.pop() {
            for entry in fs::read_dir(path).expect("read durable artifact directory") {
                let entry = entry.expect("read durable artifact entry");
                let file_type = entry.file_type().expect("durable artifact file type");
                if file_type.is_dir() {
                    pending.push(entry.path());
                } else if file_type.is_file() {
                    files.push(entry.path());
                }
            }
        }
        files
    }

    #[test]
    fn run_managed_process_egress_allows_proxy_and_blocks_direct_bypass() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let allowed = TcpListener::bind(("127.0.0.1", 0)).expect("allowed listener");
        let allowed_port = allowed.local_addr().unwrap().port();
        let denied = TcpListener::bind(("127.0.0.1", 0)).expect("denied listener");
        let denied_port = denied.local_addr().unwrap().port();
        denied.set_nonblocking(true).unwrap();
        let secret = "process-egress-secret-must-not-persist";

        let server = thread::spawn(move || {
            let (mut stream, _) = allowed.accept().expect("proxy reaches allowed listener");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).expect("read request");
                assert_ne!(read, 0, "request ended before headers");
                request.extend_from_slice(&chunk[..read]);
            }
            let request = String::from_utf8(request).expect("request is UTF-8");
            assert!(
                request.contains("Authorization: Bearer process-egress-secret-must-not-persist"),
                "the allowed request must reach the canonical proxy path: {request}"
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .expect("write response");
        });

        let script = root.path().join("managed_process_egress.harn");
        fs::write(
            &script,
            format!(
                r#"fn main(harness: Harness) {{
  harness.net.egress_policy({{allow: ["127.0.0.1:{allowed_port}"], default: "deny", block_private: "off"}})
  const direct = harness.process.run({{
    program: "/usr/bin/python3",
    args: ["-c", "import socket; socket.create_connection(('127.0.0.1', {denied_port}), 1)"]
  }})
  assert(!direct.success)
  const allowed = harness.process.run({{
    program: "/usr/bin/curl",
    args: ["--silent", "--show-error", "--header", "Authorization: Bearer " + (harness.env.get("EGRESS_TEST_SECRET") ?? ""), "http://127.0.0.1:{allowed_port}/allowed"]
  }})
  assert(allowed.success)
  assert_eq(allowed.stdout, "ok")
  harness.stdio.println("managed-egress-ok")
}}
"#
            ),
        )
        .expect("write script");

        let output = isolated_harn(root.path())
            .env("EGRESS_TEST_SECRET", secret)
            .args(["run", "--allow-process-network"])
            .arg(&script)
            .output()
            .expect("spawn harn");

        assert!(
            output.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        server.join().expect("allowed server");
        assert!(String::from_utf8_lossy(&output.stdout).contains("managed-egress-ok"));
        assert!(
            matches!(denied.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "the direct non-allowlisted listener must observe no connection"
        );

        for bytes in [&output.stdout, &output.stderr] {
            assert!(
                !String::from_utf8_lossy(bytes).contains(secret),
                "proxy diagnostics must not persist secret headers"
            );
        }
        for path in files_beneath(root.path()) {
            if path != script {
                let bytes = fs::read(&path).expect("read durable run file");
                assert!(
                    !String::from_utf8_lossy(&bytes).contains(secret),
                    "secret leaked to {}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn missing_managed_process_policy_denies_before_destination_connect() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let destination = TcpListener::bind(("127.0.0.1", 0)).expect("destination listener");
        let port = destination.local_addr().unwrap().port();
        destination.set_nonblocking(true).unwrap();
        let script = root.path().join("missing_process_egress_policy.harn");
        fs::write(
            &script,
            format!(
                r#"fn main(harness: Harness) {{
  const denied = harness.process.run({{
    program: "/usr/bin/curl",
    args: ["--fail", "--silent", "http://127.0.0.1:{port}/missing"]
  }})
  assert(!denied.success)
  harness.stdio.println("missing-policy-denied")
}}
"#
            ),
        )
        .expect("write script");

        let output = isolated_harn(root.path())
            .args(["run", "--allow-process-network"])
            .arg(&script)
            .output()
            .expect("spawn harn");

        assert!(
            output.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("missing-policy-denied"));
        assert!(
            matches!(destination.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "missing policy must not reach the destination"
        );
    }

    #[test]
    fn loopback_only_process_network_binds_locally_and_denies_remote_sockets() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let python = r#"import errno
import socket

for family, host in [(socket.AF_INET, "127.0.0.1"), (socket.AF_INET6, "::1")]:
    listener = socket.socket(family, socket.SOCK_STREAM)
    listener.bind((host, 0))
    listener.listen(1)
    client = socket.socket(family, socket.SOCK_STREAM)
    client.connect(listener.getsockname())
    accepted, _ = listener.accept()
    accepted.close()
    client.close()
    listener.close()

remote = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
remote.settimeout(0.25)
try:
    remote.connect(("198.51.100.1", 9))
except OSError as error:
    assert error.errno == errno.EPERM, error
else:
    raise AssertionError("remote socket unexpectedly connected")
finally:
    remote.close()
"#;
        let script = root.path().join("loopback_only_process_network.harn");
        fs::write(
            &script,
            format!(
                r#"fn main(harness: Harness) {{
  const probe = harness.process.run({{
    program: "/usr/bin/python3",
    args: ["-c", {}]
  }})
  assert(probe.success)
}}
"#,
                serde_json::to_string(python).expect("encode Python probe")
            ),
        )
        .expect("write script");

        let output = isolated_harn(root.path())
            .args(["run", "--allow-process-network", "--allow-process-loopback"])
            .arg(&script)
            .output()
            .expect("spawn harn");

        assert!(
            output.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn malformed_managed_process_policy_fails_before_script_execution() {
        let root = tempfile::TempDir::new().expect("tempdir");
        let marker = root.path().join("executed");
        let script = root.path().join("must_not_run.harn");
        fs::write(
            &script,
            format!(
                "fn main(harness: Harness) {{ harness.fs.write_text({}, \"bad\") }}\n",
                serde_json::to_string(&marker.to_string_lossy()).unwrap()
            ),
        )
        .expect("write script");

        let output = isolated_harn(root.path())
            .env("HARN_EGRESS_ALLOW", "[broken")
            .env("HARN_EGRESS_DEFAULT", "deny")
            .args(["run", "--allow-process-network"])
            .arg(&script)
            .output()
            .expect("spawn harn");

        assert!(!output.status.success());
        assert!(
            !marker.exists(),
            "malformed policy must fail before execution"
        );
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(
            diagnostics.contains("invalid bracketed host rule"),
            "{diagnostics}"
        );
    }
}
