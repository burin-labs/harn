#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tempfile::TempDir;

mod test_util;

use test_util::process::{harn_e2e_command, run_harn_e2e};

fn wait_for_pid(path: &std::path::Path, supervisor: &mut std::process::Child) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(raw) = fs::read_to_string(path) {
            return raw.trim().parse().expect("parse Cargo PID");
        }
        if Instant::now() >= deadline {
            let _ = supervisor.kill();
            let _ = supervisor.wait();
            panic!("Cargo start handshake timed out");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

struct ExactProcessGuard(Option<u32>);

impl Drop for ExactProcessGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }
}

fn wait_for_exit(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if unsafe { libc::kill(pid as i32, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Cargo process {pid} remained live after SIGKILL"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn abrupt_supervisor_loss_preserves_worker_lease_until_cargo_exits() {
    let temp = TempDir::new().expect("create temp directory");
    let workspace = temp.path().join("workspace");
    let target_dir = temp.path().join("target");
    let lease_root = temp.path().join("leases");
    let fake_bin = temp.path().join("bin");
    let cargo_pid_path = temp.path().join("cargo.pid");
    fs::create_dir_all(&workspace).expect("create workspace");
    fs::create_dir_all(&fake_bin).expect("create fake executable directory");
    let fake_cargo = fake_bin.join("cargo");
    fs::write(
        &fake_cargo,
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$$\" > \"$HARN_TEST_CARGO_PID_FILE\"\nexec sleep 300\n",
    )
    .expect("write fake Cargo executable");
    fs::set_permissions(&fake_cargo, fs::Permissions::from_mode(0o755))
        .expect("make fake Cargo executable");
    let mut paths = vec![fake_bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).expect("construct fake Cargo PATH");

    let mut command = harn_e2e_command();
    command
        .args([
            "host",
            "lease",
            "run",
            "cargo",
            "--owner",
            "abrupt-supervisor-test",
            "--host",
            "lease-cli-crash-fixture",
            "--workspace",
        ])
        .arg(&workspace)
        .arg("--target-dir")
        .arg(&target_dir)
        .args(["--", "test"])
        .env(harn_hostlib::HOST_LEASE_ROOT_ENV, &lease_root)
        .env("CARGO_TARGET_DIR", &target_dir)
        .env("HARN_TEST_CARGO_PID_FILE", &cargo_pid_path)
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut supervisor = command.spawn().expect("spawn supervised Cargo run");
    let cargo_pid = wait_for_pid(&cargo_pid_path, &mut supervisor);
    let mut cargo_guard = ExactProcessGuard(Some(cargo_pid));

    assert_eq!(
        unsafe { libc::kill(supervisor.id() as i32, libc::SIGKILL) },
        0,
        "kill the exact supervisor"
    );
    supervisor.wait().expect("reap killed supervisor");
    assert_eq!(
        unsafe { libc::kill(cargo_pid as i32, 0) },
        0,
        "Cargo must remain live after abrupt supervisor loss"
    );

    let lease_root_arg = lease_root.to_string_lossy();
    let owner_pid = std::process::id().to_string();
    let active = run_harn_e2e(
        &[
            "host",
            "lease",
            "status",
            "--host",
            "lease-cli-crash-fixture",
            "--resource-class",
            "rust-heavy",
            "--json",
        ],
        &[(harn_hostlib::HOST_LEASE_ROOT_ENV, lease_root_arg.as_ref())],
    );
    assert_eq!(active.exit_code, 0, "status stderr: {}", active.stderr);
    let active: serde_json::Value =
        serde_json::from_str(&active.stdout).expect("parse active lease status");
    assert_eq!(active["data"]["active"]["owner_pid"], cargo_pid);
    assert_eq!(active["data"]["active"]["priority_class"], "ci-verify");

    let deferred = run_harn_e2e(
        &[
            "host",
            "lease",
            "acquire",
            "--host",
            "lease-cli-crash-fixture",
            "--resource-class",
            "rust-heavy",
            "--owner",
            "crash-contender",
            "--no-expiry",
            "--owner-pid",
            &owner_pid,
            "--json",
        ],
        &[(harn_hostlib::HOST_LEASE_ROOT_ENV, lease_root_arg.as_ref())],
    );
    assert_eq!(
        deferred.exit_code, 75,
        "contender stderr: {}",
        deferred.stderr
    );

    assert_eq!(
        unsafe { libc::kill(cargo_pid as i32, libc::SIGKILL) },
        0,
        "kill the exact Cargo process"
    );
    wait_for_exit(cargo_pid);
    cargo_guard.0 = None;

    let recovered = run_harn_e2e(
        &[
            "host",
            "lease",
            "acquire",
            "--host",
            "lease-cli-crash-fixture",
            "--resource-class",
            "rust-heavy",
            "--owner",
            "recovered-contender",
            "--no-expiry",
            "--owner-pid",
            &owner_pid,
            "--json",
        ],
        &[(harn_hostlib::HOST_LEASE_ROOT_ENV, lease_root_arg.as_ref())],
    );
    assert_eq!(
        recovered.exit_code, 0,
        "recovery stderr: {}",
        recovered.stderr
    );
    let envelope: serde_json::Value =
        serde_json::from_str(&recovered.stdout).expect("parse recovered lease receipt");
    assert_eq!(envelope["data"]["status"], "acquired");
    let lease_id = envelope["data"]["handle"]["lease_id"]
        .as_str()
        .expect("recovered lease ID");
    let released = run_harn_e2e(
        &[
            "host",
            "lease",
            "release",
            "--host",
            "lease-cli-crash-fixture",
            "--resource-class",
            "rust-heavy",
            "--lease-id",
            lease_id,
            "--json",
        ],
        &[(harn_hostlib::HOST_LEASE_ROOT_ENV, lease_root_arg.as_ref())],
    );
    assert_eq!(released.exit_code, 0, "release stderr: {}", released.stderr);
}
