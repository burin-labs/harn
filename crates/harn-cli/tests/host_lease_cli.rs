use std::fs;

use tempfile::TempDir;

mod test_util;

use test_util::process::run_harn_e2e;

#[test]
fn host_lease_store_initialization_failure_preserves_json_contract() {
    let temp = TempDir::new().expect("create temp directory");
    let invalid_root = temp.path().join("not-a-directory");
    fs::write(&invalid_root, "file blocks lease directory creation")
        .expect("create invalid lease root");

    let invalid_root = invalid_root.to_string_lossy();
    let output = run_harn_e2e(
        &["host", "lease", "status", "--json"],
        &[(harn_hostlib::HOST_LEASE_ROOT_ENV, invalid_root.as_ref())],
    );

    assert_eq!(output.exit_code, 1);
    assert!(
        output.stderr.is_empty(),
        "JSON mode must not emit an unstructured stderr error: {}",
        output.stderr
    );
    let envelope: serde_json::Value =
        serde_json::from_str(&output.stdout).expect("failure output is a JSON envelope");
    assert_eq!(envelope["schemaVersion"], 2);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "host_lease_store");
}

#[test]
fn supervised_cargo_run_releases_the_worker_owned_rust_heavy_lease() {
    let temp = TempDir::new().expect("create temp directory");
    let workspace = temp.path().join("workspace");
    let target_dir = temp.path().join("target");
    let build_dir = temp.path().join("build");
    let lease_root = temp.path().join("leases");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::create_dir_all(&target_dir).expect("create target directory");
    fs::create_dir_all(&build_dir).expect("create build directory");

    let workspace = workspace.to_string_lossy();
    let target_dir = target_dir.to_string_lossy();
    let build_dir = build_dir.to_string_lossy();
    let lease_root = lease_root.to_string_lossy();
    let run = run_harn_e2e(
        &[
            "host",
            "lease",
            "run",
            "cargo",
            "--owner",
            "host-lease-cli-test",
            "--host",
            "lease-cli-fixture",
            "--priority-class",
            "interactive",
            "--workspace",
            workspace.as_ref(),
            "--target-dir",
            target_dir.as_ref(),
            "--build-dir",
            build_dir.as_ref(),
            "--",
            "--version",
        ],
        &[
            (harn_hostlib::HOST_LEASE_ROOT_ENV, lease_root.as_ref()),
            ("CARGO_TARGET_DIR", target_dir.as_ref()),
            ("CARGO_BUILD_BUILD_DIR", build_dir.as_ref()),
        ],
    );
    assert_eq!(run.exit_code, 0, "cargo stderr: {}", run.stderr);

    let receipt_directory = temp.path().join("leases/receipts");
    let mut entries = fs::read_dir(receipt_directory)
        .expect("read receipt directory")
        .map(|entry| entry.expect("receipt directory entry").path());
    let receipt_path = entries.next().expect("one terminal receipt");
    assert!(entries.next().is_none(), "one workload emits one receipt");
    let receipt: harn_hostlib::HostLeaseRunReceipt =
        serde_json::from_slice(&fs::read(receipt_path).expect("read terminal receipt"))
            .expect("parse terminal receipt");
    assert_eq!(
        receipt.resource.resource_class,
        harn_hostlib::HostLeaseResourceClass::RustHeavy
    );
    assert_eq!(receipt.owner, "host-lease-cli-test");
    assert_eq!(
        receipt.priority_class,
        harn_hostlib::HostLeasePriorityClass::Interactive
    );
    assert!(matches!(
        receipt.status,
        harn_hostlib::HostLeaseRunState::Completed {
            release: harn_hostlib::HostLeaseRunReleaseOutcome::Released,
            ..
        }
    ));
    assert!(receipt.execution_context.cargo_context().is_some());

    let status = run_harn_e2e(
        &[
            "host",
            "lease",
            "status",
            "--host",
            "lease-cli-fixture",
            "--resource-class",
            "rust-heavy",
            "--json",
        ],
        &[(harn_hostlib::HOST_LEASE_ROOT_ENV, lease_root.as_ref())],
    );
    assert_eq!(status.exit_code, 0, "status stderr: {}", status.stderr);
    let envelope: serde_json::Value =
        serde_json::from_str(&status.stdout).expect("status JSON envelope");
    assert_eq!(envelope["data"]["active"], serde_json::Value::Null);
}

#[test]
fn supervised_cargo_run_defers_before_invoking_cargo_when_rust_heavy_is_held() {
    let temp = TempDir::new().expect("create temp directory");
    let workspace = temp.path().join("workspace");
    let target_dir = temp.path().join("target");
    let build_dir = temp.path().join("build");
    let lease_root = temp.path().join("leases");
    fs::create_dir_all(&workspace).expect("create workspace");
    let workspace = workspace.to_string_lossy();
    let target_dir = target_dir.to_string_lossy();
    let build_dir = build_dir.to_string_lossy();
    let lease_root = lease_root.to_string_lossy();
    let owner_pid = std::process::id().to_string();

    let held = run_harn_e2e(
        &[
            "host",
            "lease",
            "acquire",
            "--host",
            "lease-cli-fixture",
            "--resource-class",
            "rust-heavy",
            "--owner",
            "test-holder",
            "--no-expiry",
            "--owner-pid",
            &owner_pid,
            "--json",
        ],
        &[(harn_hostlib::HOST_LEASE_ROOT_ENV, lease_root.as_ref())],
    );
    assert_eq!(held.exit_code, 0, "acquire stderr: {}", held.stderr);
    let held: serde_json::Value = serde_json::from_str(&held.stdout).expect("acquire JSON");
    let lease_id = held["data"]["handle"]["lease_id"]
        .as_str()
        .expect("acquired lease id")
        .to_string();

    let run = run_harn_e2e(
        &[
            "host",
            "lease",
            "run",
            "cargo",
            "--owner",
            "blocked-runner",
            "--host",
            "lease-cli-fixture",
            "--workspace",
            workspace.as_ref(),
            "--target-dir",
            target_dir.as_ref(),
            "--build-dir",
            build_dir.as_ref(),
            "--",
            "--this-cargo-argument-is-invalid",
        ],
        &[
            (harn_hostlib::HOST_LEASE_ROOT_ENV, lease_root.as_ref()),
            ("CARGO_TARGET_DIR", target_dir.as_ref()),
            ("CARGO_BUILD_BUILD_DIR", build_dir.as_ref()),
        ],
    );
    assert_eq!(run.exit_code, 75, "worker stderr: {}", run.stderr);
    let receipt_directory = temp.path().join("leases/receipts");
    let receipt_path = fs::read_dir(receipt_directory)
        .expect("read durable run receipts")
        .next()
        .expect("deferred run receipt")
        .expect("read deferred receipt entry")
        .path();
    let receipt: harn_hostlib::HostLeaseRunReceipt =
        serde_json::from_slice(&fs::read(receipt_path).expect("read deferred receipt"))
            .expect("parse deferred receipt");
    assert!(matches!(
        receipt.status,
        harn_hostlib::HostLeaseRunState::Deferred { .. }
    ));

    let released = run_harn_e2e(
        &[
            "host",
            "lease",
            "release",
            "--host",
            "lease-cli-fixture",
            "--resource-class",
            "rust-heavy",
            "--lease-id",
            &lease_id,
            "--json",
        ],
        &[(harn_hostlib::HOST_LEASE_ROOT_ENV, lease_root.as_ref())],
    );
    assert_eq!(released.exit_code, 0, "release stderr: {}", released.stderr);
}

#[test]
fn supervised_cargo_run_rejects_a_conflicting_artifact_environment() {
    let temp = TempDir::new().expect("create temp directory");
    let workspace = temp.path().join("workspace");
    let target_dir = temp.path().join("target");
    let conflicting_target = temp.path().join("other-target");
    let lease_root = temp.path().join("leases");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::create_dir_all(&conflicting_target).expect("create conflicting target");

    let workspace = workspace.to_string_lossy();
    let target_dir = target_dir.to_string_lossy();
    let conflicting_target = conflicting_target.to_string_lossy();
    let lease_root = lease_root.to_string_lossy();
    let run = run_harn_e2e(
        &[
            "host",
            "lease",
            "run",
            "cargo",
            "--owner",
            "artifact-mismatch-test",
            "--workspace",
            workspace.as_ref(),
            "--target-dir",
            target_dir.as_ref(),
            "--",
            "--version",
        ],
        &[
            (harn_hostlib::HOST_LEASE_ROOT_ENV, lease_root.as_ref()),
            ("CARGO_TARGET_DIR", conflicting_target.as_ref()),
        ],
    );

    assert_eq!(run.exit_code, 1);
    assert!(run.stderr.contains("CARGO_TARGET_DIR disagrees"));
    assert!(
        !temp.path().join("leases/receipts").exists(),
        "a rejected artifact identity must not create a run receipt"
    );
}

#[cfg(unix)]
#[test]
fn cancelling_the_supervisor_reaps_cargo_before_releasing_its_lease() {
    use std::io::{BufRead, Read};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::Duration;

    let temp = TempDir::new().expect("create temp directory");
    let workspace = temp.path().join("workspace");
    let target_dir = temp.path().join("target");
    let lease_root = temp.path().join("leases");
    let fake_bin = temp.path().join("bin");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::create_dir_all(&fake_bin).expect("create fake executable directory");
    let fake_cargo = fake_bin.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\nset -eu\nif env | grep -Eq '^HARN_CARGO_LEASE_(RUNNER|OWNER|HOST|WAIT_MS|PRIORITY_CLASS)='; then\n  echo 'lease control environment reached Cargo' >&2\n  exit 91\nfi\nprintf 'HARN_HOST_LEASE_CARGO_STARTED=%s\\n' \"$$\"\nexec sleep 300\n",
    )
    .expect("write fake Cargo executable");
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755))
        .expect("make fake Cargo executable");

    let mut paths = vec![fake_bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).expect("construct fake Cargo PATH");
    let mut command = test_util::process::harn_e2e_command();
    command
        .args([
            "host",
            "lease",
            "run",
            "cargo",
            "--owner",
            "cancellation-test",
            "--host",
            "lease-cli-cancel-fixture",
            "--workspace",
        ])
        .arg(&workspace)
        .arg("--target-dir")
        .arg(&target_dir)
        .args(["--", "test"])
        .env(harn_hostlib::HOST_LEASE_ROOT_ENV, &lease_root)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("HARN_CARGO_LEASE_RUNNER", "must-not-reach-cargo")
        .env("HARN_CARGO_LEASE_OWNER", "must-not-reach-cargo")
        .env("HARN_CARGO_LEASE_HOST", "must-not-reach-cargo")
        .env("HARN_CARGO_LEASE_WAIT_MS", "999")
        .env("HARN_CARGO_LEASE_PRIORITY_CLASS", "measurement")
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut supervisor = command.spawn().expect("spawn supervised Cargo run");
    let stdout = supervisor.stdout.take().expect("capture supervisor stdout");
    let (started_tx, started_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut stdout = std::io::BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match stdout.read_line(&mut line) {
                Ok(0) => {
                    let _ =
                        started_tx.send(Err("supervisor exited before Cargo started".to_string()));
                    return;
                }
                Ok(_) => {
                    if let Some(pid) = line.trim().strip_prefix("HARN_HOST_LEASE_CARGO_STARTED=") {
                        let _ = started_tx.send(
                            pid.parse::<u32>()
                                .map_err(|error| format!("invalid Cargo PID: {error}")),
                        );
                        return;
                    }
                }
                Err(error) => {
                    let _ = started_tx.send(Err(format!("read Cargo handshake: {error}")));
                    return;
                }
            }
        }
    });
    let cargo_pid = match started_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(pid)) => pid,
        Ok(Err(error)) => {
            let _ = supervisor.kill();
            let _ = supervisor.wait();
            panic!("{error}");
        }
        Err(error) => {
            let _ = supervisor.kill();
            let _ = supervisor.wait();
            panic!("Cargo start handshake timed out: {error}");
        }
    };

    let signal_result = unsafe { libc::kill(supervisor.id() as i32, libc::SIGTERM) };
    assert_eq!(signal_result, 0, "signal the exact supervisor process");
    let status = supervisor.wait().expect("wait for cancelled supervisor");
    let mut stderr = String::new();
    supervisor
        .stderr
        .take()
        .expect("capture supervisor stderr")
        .read_to_string(&mut stderr)
        .expect("read supervisor stderr");
    reader.join().expect("join Cargo handshake reader");
    assert_eq!(status.code(), Some(130), "supervisor stderr: {stderr}");

    let cargo_liveness = unsafe { libc::kill(cargo_pid as i32, 0) };
    assert_eq!(
        cargo_liveness, -1,
        "Cargo process {cargo_pid} survived cancellation"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );

    let receipt_path = fs::read_dir(lease_root.join("receipts"))
        .expect("read cancellation receipt directory")
        .next()
        .expect("one cancellation receipt")
        .expect("read cancellation receipt entry")
        .path();
    let receipt: harn_hostlib::HostLeaseRunReceipt =
        serde_json::from_slice(&fs::read(receipt_path).expect("read cancellation receipt"))
            .expect("parse cancellation receipt");
    assert!(matches!(
        receipt.status,
        harn_hostlib::HostLeaseRunState::Cancelled {
            release: harn_hostlib::HostLeaseRunReleaseOutcome::Released,
            worker_pid,
            ..
        } if worker_pid == cargo_pid
    ));

    let state = harn_hostlib::HostLeaseStore::for_root(&lease_root)
        .expect("open lease store")
        .status_for_resource(
            "lease-cli-cancel-fixture",
            harn_hostlib::HostLeaseResourceClass::RustHeavy,
        )
        .expect("inspect released rust-heavy resource");
    assert!(state.active.is_none());
}
