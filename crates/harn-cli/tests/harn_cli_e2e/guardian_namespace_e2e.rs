#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use crate::test_util::process::{harn_e2e_binary, harn_e2e_command};

const LAUNCHER_ENV: &str = "HARN_EXT_OWNER_DEATH_NETNS_LAUNCHER";

const PROBE: &str = r#"
import ctypes, errno, os, signal, socket, subprocess, sys
for host in ['127.0.0.1', '::1']:
    family = socket.AF_INET6 if ':' in host else socket.AF_INET
    listener = socket.socket(family, socket.SOCK_STREAM)
    listener.bind((host, 0))
    listener.listen(1)
    client = socket.socket(family, socket.SOCK_STREAM)
    client.connect(listener.getsockname())
    accepted, _ = listener.accept()
    accepted.close(); client.close(); listener.close()
for kind in [socket.SOCK_STREAM, socket.SOCK_DGRAM]:
    remote = socket.socket(socket.AF_INET, kind)
    try:
        if kind == socket.SOCK_STREAM:
            remote.connect(('198.51.100.1', 9))
        else:
            remote.sendto(b'no egress', ('198.51.100.1', 9))
    except OSError as error:
        assert error.errno == errno.ENETUNREACH, error
    else:
        raise AssertionError('off-host egress succeeded')
    remote.close()
try:
    os.listdir('/')
except PermissionError:
    pass
else:
    raise AssertionError('Landlock confinement missing')
libc = ctypes.CDLL(None, use_errno=True)
assert libc.unshare(0) == -1
assert ctypes.get_errno() == errno.EPERM, 'seccomp ceiling missing'
if len(sys.argv) == 1:
    print('loopback-egress-landlock-seccomp-ok', flush=True)
else:
    grandchild = subprocess.Popen([sys.executable, '-c', "import signal; print('ready', flush=True); signal.pause()"], start_new_session=True, stdout=subprocess.PIPE)
    assert grandchild.stdout.readline() == b'ready\n'
    with open(sys.argv[1], 'w') as report:
        report.write(str(os.getpid()) + ' ' + str(os.getpgrp()) + ' ' + str(grandchild.pid) + '\n')
        report.flush()
    signal.pause()
"#;

pub(super) struct Owner(pub(super) Child);

impl Drop for Owner {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

pub(super) fn pidfd(pid: i32) -> OwnedFd {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    assert!(
        fd >= 0,
        "open live process {pid}: {}",
        std::io::Error::last_os_error()
    );
    unsafe { OwnedFd::from_raw_fd(fd) }
}

pub(super) fn await_exit(fd: &OwnedFd) {
    let mut event = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&raw mut event, 1, 10_000) },
        1,
        "owner death did not kill child"
    );
    assert_ne!(event.revents & libc::POLLIN, 0);
}

#[test]
fn owner_death_preserves_namespace_then_confinement() {
    let configured = std::env::var(LAUNCHER_ENV).ok();
    if !harn_vm::process_sandbox::active_backend_filesystem_available() && configured.is_none() {
        eprintln!(
            "NOT EXERCISED: Landlock unavailable; set {LAUNCHER_ENV} to require the Linux proof."
        );
        return;
    }
    let launcher = configured
        .clone()
        .unwrap_or_else(|| harn_e2e_binary().display().to_string());
    // Probe namespace authority independently. An explicitly supplied helper
    // must work; ordinary runners can lack the host's per-executable grant.
    let preflight = Command::new(&launcher)
        .args([
            "netns-launch",
            "--seccomp-hex",
            "060000000000ff7f",
            "--",
            "/usr/bin/python3",
            "-c",
            "import ctypes; assert ctypes.CDLL(None).unshare(0) == 0",
        ])
        .output()
        .expect("probe namespace helper authority");
    if !preflight.status.success() && configured.is_none() {
        eprintln!("NOT EXERCISED: namespace authority unavailable; set {LAUNCHER_ENV} to require the Linux proof. {}", String::from_utf8_lossy(&preflight.stderr));
        return;
    }
    assert!(
        preflight.status.success(),
        "namespace preflight: {}",
        String::from_utf8_lossy(&preflight.stderr)
    );

    let root = tempfile::tempdir().unwrap();
    let fifo = root.path().join("ready.fifo");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let report = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&fifo)
        .unwrap();
    let source = format!(
        r#"
fn main(harness: Harness) {{
  const probe = {probe}
  const direct = harness.tools.run_command({{argv: ["/usr/bin/python3", "-c", probe]}})
  assert_eq(direct.exit_code, 0)
  assert_eq(trim(direct.stdout), "loopback-egress-landlock-seccomp-ok")
  const child = harness.tools.run_command({{argv: ["/usr/bin/python3", "-c", probe, {fifo}], background: true}})
  const result = harness.tools.wait_command({{handle_id: child.handle_id ?? "", timeout_ms: 60000}})
  throw json_stringify(result)
}}
"#,
        probe = serde_json::to_string(PROBE).unwrap(),
        fifo = serde_json::to_string(&fifo.display().to_string()).unwrap()
    );
    let mut owner = Owner(
        harn_e2e_command()
            .current_dir(root.path())
            .args([
                "run",
                "--standalone",
                "--allow-process-loopback",
                "--netns-launcher",
                &launcher,
                "-e",
                &source,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0)
            .spawn()
            .expect("start canonical CLI owner"),
    );

    let mut readiness = libc::pollfd {
        fd: report.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&raw mut readiness, 1, 30_000) },
        1,
        "confined payload never reported readiness"
    );
    let mut line = String::new();
    BufReader::new(report).read_line(&mut line).unwrap();
    let fields = line
        .split_whitespace()
        .map(|v| v.parse::<i32>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        fields.len(),
        3,
        "payload probe must fire before owner death"
    );
    let child = pidfd(fields[0]);
    let grandchild = pidfd(fields[2]);
    for live in [&child, &grandchild] {
        let mut event = libc::pollfd {
            fd: live.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&raw mut event, 1, 0) },
            0,
            "child must be live before owner death"
        );
    }
    assert_ne!(
        fields[1],
        owner.0.id() as i32,
        "guardian must outlive owner process group"
    );
    assert_eq!(
        unsafe { libc::kill(-(owner.0.id() as i32), libc::SIGKILL) },
        0
    );
    owner.0.wait().unwrap();
    await_exit(&child);
    await_exit(&grandchild);

    use harn_vm::orchestration::{CapabilityPolicy, SandboxProfile};
    let mut policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::OsHardened,
        workspace_roots: vec![root.path().display().to_string()],
        ..CapabilityPolicy::default()
    };
    policy.process_sandbox.allow_tcp_loopback = true;
    policy.process_sandbox.netns_launcher_path = Some(launcher);
    harn_vm::orchestration::push_execution_policy(policy);
    let mut early = harn_vm::process_sandbox::std_command_for("/usr/bin/true", &[]).unwrap();
    let confinement = harn_vm::process_sandbox::transferable_confinement("/usr/bin/true")
        .unwrap()
        .unwrap();
    harn_vm::orchestration::pop_execution_policy();
    // Negative control restores the broken order using only sandbox syscalls
    // in the forked child. Namespace creation must fail before true executes.
    unsafe {
        early.pre_exec(move || confinement.enter());
    }
    let output = early
        .output()
        .expect("start deliberately pre-confined helper");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("could not build a private network namespace: Operation not permitted"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
