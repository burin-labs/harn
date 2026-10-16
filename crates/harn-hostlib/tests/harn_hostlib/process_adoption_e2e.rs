//! The real guardian must reap adoptees even when inherited confinement hides /proc enumeration.
#![cfg(target_os = "linux")]

use std::io::{BufRead, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use harn_hostlib::process::{spawn_process, EnvMode, OutputCapture, OwnerDeathPolicy, SpawnSpec};
use harn_vm::orchestration::{
    pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
};

const ROLE: &str = "HARN_TEST_ADOPTION_ROLE";
const FIXTURE: &str = "process_adoption_e2e::adoption_fixture";

fn fixture_command(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", FIXTURE, "--nocapture"])
        .env(ROLE, role);
    command
}

fn report(value: serde_json::Value) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}

fn read_report(reader: &mut impl BufRead) -> serde_json::Value {
    loop {
        let mut line = String::new();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "fixture ended before its barrier"
        );
        if line.starts_with('{') {
            return serde_json::from_str(&line).unwrap();
        }
    }
}

fn leaf() -> ! {
    report(serde_json::json!({"pid": std::process::id()}));
    loop {
        unsafe {
            libc::pause();
        }
    }
}

fn start_leaf() -> std::process::Child {
    let mut child = fixture_command("leaf")
        .process_group(0)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let ready = read_report(&mut std::io::BufReader::new(child.stdout.take().unwrap()));
    assert_eq!(ready["pid"], child.id());
    child
}

#[test]
fn adoption_fixture() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    if role == "leaf" {
        leaf();
    }
    if role == "payload" {
        let child = start_leaf();
        report(serde_json::json!({"payload": std::process::id(), "adoptee": child.id()}));
        std::mem::forget(child);
        let mut released = [0];
        std::io::stdin().read_exact(&mut released).unwrap();
        return;
    }
    assert_eq!(role, "supervisor");
    let _environment = super::process_tools_e2e::declare_inherited();
    let _guardian = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "process_tools_e2e::owner_death_guardian_fixture",
        "--nocapture",
    ]);
    assert_eq!(
        std::fs::read_dir("/proc")
            .expect_err("inherited confinement must deny enumeration")
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let mut sibling = start_leaf();
    let sibling_birth = birth(sibling.id());
    let mut managed = spawn_process(spec("payload", OwnerDeathPolicy::KillContainment)).unwrap();
    let guardian_pid = managed.pid().unwrap();
    let mut stdout = std::io::BufReader::new(managed.take_stdout().unwrap());
    let ready = read_report(&mut stdout);
    report(
        serde_json::json!({"guardian": guardian_pid, "payload": ready["payload"],
        "adoptee": ready["adoptee"], "sibling": sibling.id()}),
    );
    // The unconfined test controller binds pidfds before releasing the payload.
    std::io::stdin().read_exact(&mut [0]).unwrap();
    managed.take_stdin().unwrap().write_all(b"finish").unwrap();
    let terminal = managed.wait().unwrap();
    assert_eq!(
        terminal.code,
        Some(0),
        "actual guardian must preserve payload success"
    );
    let adoptee = ready["adoptee"].as_i64().unwrap() as i32;
    assert_eq!(unsafe { libc::kill(adoptee, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert!(
        sibling.try_wait().unwrap().is_none(),
        "unrelated held child survived cleanup"
    );
    assert_eq!(birth(sibling.id()), sibling_birth);
    sibling.kill().unwrap();
    sibling.wait().unwrap();
    report(
        serde_json::json!({"guardian_settled": true, "adoptee_reaped": true,
        "unrelated_survived": true, "proc_enumeration_denied": true, "pending": 0}),
    );
}

fn spec(role: &str, owner_death: OwnerDeathPolicy) -> SpawnSpec {
    SpawnSpec {
        builtin: "guardian_adoption_regression",
        program: std::env::current_exe().unwrap().display().to_string(),
        args: vec!["--exact".into(), FIXTURE.into(), "--nocapture".into()],
        cwd: None,
        env: [(ROLE.into(), role.into())].into(),
        env_remove: Vec::new(),
        env_mode: EnvMode::Replace,
        use_stdin: true,
        configure_process_group: true,
        owner_death,
        output_capture: OutputCapture::Pipe,
    }
}

fn birth(pid: u32) -> String {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .unwrap()
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .to_string()
}

/// Cleanup stays bound to the controller's measured process identities even if the regression hangs.
#[derive(Default)]
struct PidCleanup(Vec<OwnedFd>);

impl PidCleanup {
    fn retain(&mut self, pid: u32) {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        assert!(fd >= 0, "bind live fixture identity {pid}");
        self.0.push(unsafe { OwnedFd::from_raw_fd(fd as i32) });
    }
}

impl Drop for PidCleanup {
    fn drop(&mut self) {
        for fd in &self.0 {
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
    }
}

#[test]
fn confined_guardian_reaps_adopted_children_without_proc_enumeration() {
    if !harn_vm::process_sandbox::active_backend_filesystem_available() {
        assert!(
            std::env::var_os("HARN_REQUIRE_LANDLOCK_TESTS").is_none(),
            "required Landlock boundary unavailable"
        );
        eprintln!("not_measured: inherited Landlock unavailable");
        return;
    }
    let _environment = super::process_tools_e2e::declare_inherited();
    let workspace = tempfile::tempdir().unwrap();
    let mut request = spec("supervisor", OwnerDeathPolicy::None);
    request.cwd = Some(workspace.path().into());
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![workspace.path().display().to_string()],
        ..Default::default()
    });
    let launched = spawn_process(request);
    pop_execution_policy();
    let mut supervisor = launched.unwrap();
    let mut cleanup = PidCleanup::default();
    cleanup.retain(supervisor.pid().unwrap());
    let mut input = supervisor.take_stdin().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let mut stdout = std::io::BufReader::new(supervisor.take_stdout().unwrap());
    harn_parser::runtime_stack::spawn(move || {
        ready_tx.send(read_report(&mut stdout)).unwrap();
        let final_report = read_report(&mut stdout);
        done_tx.send((final_report, supervisor.wait())).unwrap();
    });
    let ready =
        harn_clock::test_support::recv_within("confined payload and guardian ready", &ready_rx);
    for name in ["guardian", "payload", "adoptee", "sibling"] {
        cleanup.retain(ready[name].as_u64().unwrap() as u32);
    }
    input.write_all(b"release").unwrap();
    let (report, terminal) =
        harn_clock::test_support::recv_within("real confined guardian terminal", &done_rx);
    assert_eq!(terminal.unwrap().code, Some(0));
    assert_eq!(
        report,
        serde_json::json!({"guardian_settled": true, "adoptee_reaped": true,
        "unrelated_survived": true, "proc_enumeration_denied": true, "pending": 0})
    );
    eprintln!("guardian_adoption_measured: {report}");
}
