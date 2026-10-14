//! Owner-death containment for managed background commands, end to end.
//!
//! A supervisor process starts a background command through the real spawner,
//! then dies. Everything the command started must die with it, including when
//! another process holds a copy of the supervisor's liveness pipe. The guardian
//! the spawner re-execs into is `process_tools_e2e::owner_death_guardian_fixture`.

#![cfg(unix)]

use std::io::{BufRead, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;

use super::process_tools_e2e::{declare_inherited, lock_env};

const OWNER_DEATH_SUPERVISOR_ENV: &str = "HARN_TEST_OWNER_DEATH_SUPERVISOR";
const OWNER_DEATH_REPORT_FD_ENV: &str = "HARN_TEST_OWNER_DEATH_REPORT_FD";
const OWNER_DEATH_LEAK_LIVENESS_ENV: &str = "HARN_TEST_OWNER_DEATH_LEAK_LIVENESS";
const OWNER_DEATH_STALLED_STDIN_ENV: &str = "HARN_TEST_OWNER_DEATH_STALLED_STDIN";
const CLOSED_INPUT_WORK_ENV: &str = "HARN_TEST_CLOSED_INPUT_WORK";

#[test]
fn closed_input_payload_fixture() {
    let Some(work) = std::env::var_os(CLOSED_INPUT_WORK_ENV) else {
        return;
    };
    assert_eq!(unsafe { libc::close(libc::STDIN_FILENO) }, 0);
    println!("payload-input-closed");
    std::io::stdout().flush().unwrap();
    let work = std::path::PathBuf::from(work);
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(harn_clock::test_support::within(
            "live owner releases child work after writing to closed input",
            async {
                while !work.join("release").exists() {
                    tokio::task::yield_now().await;
                }
            },
        ));
    std::fs::write(
        work.join("completed"),
        b"child completed after closing input",
    )
    .unwrap();
}

#[test]
fn contained_child_can_close_input_and_finish_while_owner_remains_alive() {
    use harn_hostlib::process::{
        spawn_process, EnvMode, OutputCapture, OwnerDeathPolicy, SpawnSpec,
    };
    let _environment = declare_inherited();
    let _guardian_args = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "process_tools_e2e::owner_death_guardian_fixture",
        "--nocapture",
    ]);
    let work = tempfile::tempdir().unwrap();
    let mut child = spawn_process(SpawnSpec {
        builtin: "closed_input_live_owner_fixture",
        program: std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        args: vec![
            "--exact".into(),
            "process_owner_death_e2e::closed_input_payload_fixture".into(),
            "--nocapture".into(),
        ],
        cwd: None,
        env: [(
            CLOSED_INPUT_WORK_ENV.into(),
            work.path().to_string_lossy().into_owned(),
        )]
        .into(),
        env_remove: Vec::new(),
        env_mode: EnvMode::Patch,
        use_stdin: true,
        configure_process_group: true,
        owner_death: OwnerDeathPolicy::KillContainment,
        output_capture: OutputCapture::Pipe,
    })
    .unwrap();
    let _cleanup = ProcessGroupCleanup {
        groups: vec![child.process_group_id().unwrap() as i32],
    };
    let mut stdout = std::io::BufReader::new(child.take_stdout().unwrap());
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    harn_parser::runtime_stack::spawn(move || {
        let mut ready_tx = Some(ready_tx);
        for line in stdout.by_ref().lines() {
            if line.unwrap() == "payload-input-closed" {
                ready_tx
                    .take()
                    .expect("one readiness marker")
                    .send(true)
                    .unwrap();
            }
        }
        if let Some(ready_tx) = ready_tx {
            let _ = ready_tx.send(false);
        }
    });
    assert!(harn_clock::test_support::recv_within(
        "actual child closed stdin",
        &ready_rx
    ));
    let mut input = child.take_stdin().unwrap();
    let (written_tx, written_rx) = std::sync::mpsc::channel();
    harn_parser::runtime_stack::spawn(move || {
        let result = input.write_all(&vec![0x53; 128 * 1024]);
        drop(input);
        let _ = written_tx.send(result);
    });
    harn_clock::test_support::recv_within(
        "guardian drains input after child closes it",
        &written_rx,
    )
    .expect("child input closure must not kill its live owner containment");
    std::fs::write(work.path().join("release"), b"finish").unwrap();
    assert_eq!(
        child
            .wait_with_timeout(Some(harn_clock::test_support::HANG_CEILING), &|| false)
            .unwrap(),
        harn_hostlib::process::WaitOutcome::Exited(harn_hostlib::process::ExitStatus::from_code(0)),
        "child finishes with its owner alive"
    );
    assert_eq!(
        std::fs::read(work.path().join("completed")).unwrap(),
        b"child completed after closing input"
    );
}

#[test]
fn owner_death_grandchild_fixture() {
    if std::env::var_os(OWNER_DEATH_REPORT_FD_ENV).is_none() {
        return;
    }
    loop {
        unsafe {
            libc::pause();
        }
    }
}

#[test]
fn owner_death_payload_fixture() {
    let Some(report_fd) = std::env::var(OWNER_DEATH_REPORT_FD_ENV)
        .ok()
        .and_then(|value| value.parse::<RawFd>().ok())
    else {
        return;
    };
    let grandchild = std::process::Command::new(
        std::env::current_exe().expect("resolve process-tools test executable"),
    )
    .args([
        "--exact",
        "process_owner_death_e2e::owner_death_grandchild_fixture",
        "--nocapture",
    ])
    .process_group(0)
    .spawn()
    .expect("spawn native grandchild fixture");
    let report = format!(
        "payload={} pgid={} grandchild={} grandchild_pgid={}\n",
        std::process::id(),
        unsafe { libc::getpgrp() },
        grandchild.id(),
        grandchild.id(),
    );
    let written = unsafe { libc::write(report_fd, report.as_ptr().cast(), report.len()) };
    assert_eq!(written, report.len() as isize, "write payload handshake");
    std::mem::forget(grandchild);
    loop {
        unsafe {
            libc::pause();
        }
    }
}

#[test]
fn owner_death_supervisor_fixture() {
    if std::env::var_os(OWNER_DEATH_SUPERVISOR_ENV).is_none() {
        return;
    }
    // A separate process, so it does not go through this module's `call`
    // helper and declares its own inheriting environment (harn#8477).
    let _environment = declare_inherited();
    let _guardian_args = harn_hostlib::process::owner_death::install_guardian_reexec_args([
        "--exact",
        "process_tools_e2e::owner_death_guardian_fixture",
        "--nocapture",
    ]);
    let mut input_owner = None;
    let (worker_pid, worker_pgid) = if std::env::var_os(OWNER_DEATH_STALLED_STDIN_ENV).is_some() {
        use harn_hostlib::process::{
            spawn_process, EnvMode, OutputCapture, OwnerDeathPolicy, SpawnSpec,
        };
        let mut child = spawn_process(SpawnSpec {
            builtin: "owner_death_stalled_input_fixture",
            program: std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            args: vec![
                "--exact".into(),
                "process_owner_death_e2e::owner_death_payload_fixture".into(),
                "--nocapture".into(),
            ],
            cwd: None,
            env: std::collections::BTreeMap::new(),
            env_remove: Vec::new(),
            env_mode: EnvMode::Patch,
            use_stdin: true,
            configure_process_group: true,
            owner_death: OwnerDeathPolicy::KillContainment,
            output_capture: OutputCapture::Pipe,
        })
        .expect("spawn actual contained child with application stdin");
        let pid = child.pid().expect("payload pid");
        let pgid = child.process_group_id().expect("payload group");
        wait_for_stalled_input(child.take_stdin().expect("payload input"));
        input_owner = Some(child);
        (pid, pgid)
    } else {
        let info = harn_hostlib::tools::long_running::spawn_long_running(
            "owner_death_supervisor_fixture",
            std::env::current_exe()
                .expect("resolve process-tools test executable")
                .to_string_lossy()
                .into_owned(),
            vec![
                "--exact".to_string(),
                "process_owner_death_e2e::owner_death_payload_fixture".to_string(),
                "--nocapture".to_string(),
            ],
            None,
            std::collections::BTreeMap::new(),
            format!("owner-death-supervisor-{}", std::process::id()),
        )
        .expect("spawn managed background payload");
        (
            info.pid,
            info.process_group_id.expect("worker process group"),
        )
    };
    // A sibling forked without exec keeps every descriptor this process holds,
    // including the write end of the guardian's liveness pipe. It stands in
    // for a process another thread spawned while that pipe was still
    // inheritable, which is how the owner-closed EOF went missing.
    let leaked_liveness_holder = if std::env::var_os(OWNER_DEATH_LEAK_LIVENESS_ENV).is_some() {
        // The holder must not keep the payload report open, or the test could
        // not observe the payload's descriptors closing.
        let report_fd = std::env::var(OWNER_DEATH_REPORT_FD_ENV)
            .ok()
            .and_then(|value| value.parse::<RawFd>().ok())
            .expect("payload report descriptor");
        match unsafe { libc::fork() } {
            0 => unsafe {
                libc::setpgid(0, 0);
                libc::close(report_fd);
                loop {
                    libc::pause();
                }
            },
            pid if pid > 0 => {
                // Both sides set the group so the holder is outside the
                // supervisor's group before the test SIGKILLs that group.
                unsafe {
                    libc::setpgid(pid, pid);
                }
                pid
            }
            _ => panic!("fork liveness holder: {}", std::io::Error::last_os_error()),
        }
    } else {
        0
    };
    println!(
        "supervisor={} supervisor_pgid={} worker={} worker_pgid={} holder={}",
        std::process::id(),
        unsafe { libc::getpgrp() },
        worker_pid,
        worker_pgid,
        leaked_liveness_holder,
    );
    std::io::stdout().flush().expect("flush supervisor report");
    // Retain the real process handle, including its owner-liveness writer.
    std::hint::black_box(&input_owner);
    loop {
        unsafe {
            libc::pause();
        }
    }
}

struct ProcessGroupCleanup {
    groups: Vec<i32>,
}

impl Drop for ProcessGroupCleanup {
    fn drop(&mut self) {
        for pgid in &self.groups {
            if *pgid > 0 {
                unsafe {
                    libc::kill(-*pgid, libc::SIGKILL);
                }
            }
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn managed_background_group_dies_when_its_supervisor_is_sigkilled() {
    assert_managed_background_group_dies_with_supervisor(false, false);
}

#[cfg(target_os = "linux")]
#[test]
fn contained_group_dies_when_payload_stdin_is_stalled_and_owner_is_sigkilled() {
    assert_managed_background_group_dies_with_supervisor(false, true);
    assert_managed_background_group_dies_with_supervisor(true, true);
}

/// Observe a sleeping write syscall on the actual pipe, rather than assuming
/// that a quiet period means the forwarding stages have filled. The payload
/// never reads stdin, so backpressure persists until the supervisor is killed.
/// Linux exposes the necessary thread and descriptor state through procfs.
#[cfg(target_os = "linux")]
fn wait_for_stalled_input(mut input: Box<dyn Write + Send>) {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    harn_parser::runtime_stack::spawn(move || {
        let bytes = [0_u8; 4096];
        input.write_all(&bytes).expect("first actual input write");
        started_tx
            .send(unsafe { libc::syscall(libc::SYS_gettid) })
            .unwrap();
        let result = (0..16_384).try_for_each(|_| input.write_all(&bytes));
        let _ = done_tx.send(result.is_ok());
    });
    let tid = harn_clock::test_support::recv_within("writer reached the actual pipe", &started_rx);
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(harn_clock::test_support::within(
            "application input writer sleeps in a kernel pipe write",
            async {
                loop {
                    assert!(
                        matches!(
                            done_rx.try_recv(),
                            Err(std::sync::mpsc::TryRecvError::Empty)
                        ),
                        "input writer finished before owner death"
                    );
                    let state = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat"))
                        .expect("read actual writer thread state");
                    let syscall = std::fs::read_to_string(format!("/proc/self/task/{tid}/syscall"))
                        .expect("read actual writer syscall");
                    let mut fields = syscall.split_whitespace();
                    if state
                        .rsplit_once(") ")
                        .is_some_and(|(_, tail)| tail.starts_with("S "))
                        && fields
                            .next()
                            .and_then(|value| value.parse::<libc::c_long>().ok())
                            == Some(libc::SYS_write)
                    {
                        let fd = fields.next().expect("write syscall descriptor");
                        let fd = u32::from_str_radix(fd.trim_start_matches("0x"), 16)
                            .expect("parse write syscall descriptor");
                        let pipe = std::fs::read_link(format!("/proc/self/fd/{fd}"))
                            .expect("resolve actual writer descriptor");
                        assert!(pipe.to_string_lossy().starts_with("pipe:["));
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            },
        ));
}

#[cfg(not(target_os = "linux"))]
fn wait_for_stalled_input(_input: Box<dyn Write + Send>) {
    panic!("stalled-input qualification requires Linux kernel pipe-write evidence");
}

/// The liveness pipe's EOF is not the only owner-death signal: a leaked copy
/// of its write end keeps the pipe open after the owner is gone, and the
/// background group must still die. Run as a stress loop, because the owner
/// check races the owner's exit and one clean trial would not show a lost
/// wakeup.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn managed_background_group_dies_when_a_sibling_holds_the_liveness_pipe() {
    for _ in 0..OWNER_EXIT_STRESS_TRIALS {
        assert_managed_background_group_dies_with_supervisor(true, false);
    }
}

const OWNER_EXIT_STRESS_TRIALS: usize = 20;

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn assert_managed_background_group_dies_with_supervisor(
    leak_liveness_pipe: bool,
    stalled_stdin: bool,
) {
    // Held for the whole trial. Each trial's report pipe is inheritable on
    // purpose, so a concurrent trial's supervisor would hold a copy of it, and
    // a test that rewrites TMPDIR while the supervisor spawns hands it a
    // directory that is gone by the time it starts its payload.
    let _serial = lock_env();
    let mut report_pipe = [0_i32; 2];
    assert_eq!(unsafe { libc::pipe(report_pipe.as_mut_ptr()) }, 0);
    let read_fd = report_pipe[0];
    let write_fd = report_pipe[1];
    let read_flags = unsafe { libc::fcntl(read_fd, libc::F_GETFD) };
    assert!(read_flags >= 0);
    assert_eq!(
        unsafe { libc::fcntl(read_fd, libc::F_SETFD, read_flags | libc::FD_CLOEXEC) },
        0
    );

    let mut supervisor = std::process::Command::new(
        std::env::current_exe().expect("resolve process-tools test executable"),
    );
    supervisor
        .args([
            "--exact",
            "process_owner_death_e2e::owner_death_supervisor_fixture",
            "--nocapture",
        ])
        .env(OWNER_DEATH_SUPERVISOR_ENV, "1")
        .env(OWNER_DEATH_REPORT_FD_ENV, write_fd.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .process_group(0);
    if leak_liveness_pipe {
        supervisor.env(OWNER_DEATH_LEAK_LIVENESS_ENV, "1");
    }
    if stalled_stdin {
        supervisor.env(OWNER_DEATH_STALLED_STDIN_ENV, "1");
    }
    let mut supervisor = supervisor.spawn().expect("spawn isolated supervisor");
    unsafe {
        libc::close(write_fd);
    }
    let mut cleanup = ProcessGroupCleanup {
        groups: vec![supervisor.id() as i32],
    };

    let supervisor_stdout = supervisor.stdout.take().expect("supervisor stdout");
    let mut supervisor_lines = std::io::BufReader::new(supervisor_stdout).lines();
    let supervisor_report = supervisor_lines
        .find_map(|line| {
            let line = line.expect("read supervisor report");
            line.starts_with("supervisor=").then_some(line)
        })
        .expect("supervisor report line");
    let fields = parse_pid_fields(&supervisor_report);
    let supervisor_pid = fields["supervisor"];
    let supervisor_pgid = fields["supervisor_pgid"];
    let worker_pid = fields["worker"];
    let worker_pgid = fields["worker_pgid"];
    assert_eq!(supervisor_pid, supervisor_pgid);
    assert_eq!(worker_pid, worker_pgid);
    assert_ne!(worker_pgid, supervisor_pgid);
    cleanup.groups.push(worker_pgid);
    if leak_liveness_pipe {
        assert!(fields["holder"] > 0, "liveness holder was not forked");
        cleanup.groups.push(fields["holder"]);
    }

    let mut report_file = unsafe { std::fs::File::from_raw_fd(read_fd) };
    let mut payload_report = String::new();
    std::io::BufReader::new(&mut report_file)
        .read_line(&mut payload_report)
        .expect("read payload report");
    let payload_fields = parse_pid_fields(&payload_report);
    assert_eq!(payload_fields["pgid"], worker_pgid);
    assert_ne!(payload_fields["payload"], payload_fields["grandchild"]);
    assert_eq!(
        payload_fields["grandchild"],
        payload_fields["grandchild_pgid"]
    );
    assert_ne!(payload_fields["grandchild_pgid"], worker_pgid);
    cleanup.groups.push(payload_fields["grandchild_pgid"]);

    assert_eq!(
        unsafe { libc::kill(-supervisor_pgid, libc::SIGKILL) },
        0,
        "SIGKILL isolated supervisor group"
    );
    supervisor.wait().expect("reap supervisor");

    let mut poll_fd = libc::pollfd {
        fd: read_fd,
        events: libc::POLLIN | libc::POLLHUP,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&raw mut poll_fd, 1, 10_000) },
        1,
        "worker descriptors did not close after owner death"
    );
    let mut eof = [0_u8; 1];
    assert_eq!(
        report_file.read(&mut eof).expect("read owner-death EOF"),
        0,
        "worker report descriptor remained open"
    );
    wait_for_native_exit(worker_pid);
    if unsafe { libc::kill(-worker_pgid, 0) } != -1
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    {
        // Darwin can report the just-exited group as EAGAIN between NOTE_EXIT
        // delivery and launchd reaping its orphaned leader. A second native
        // process-exit barrier closes that kernel transition without sleeping
        // or polling.
        wait_for_native_exit(worker_pid);
    }
    assert_eq!(unsafe { libc::kill(-worker_pgid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    wait_for_native_exit(payload_fields["grandchild"]);
    assert_eq!(
        unsafe { libc::kill(-payload_fields["grandchild_pgid"], 0) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    // Only the liveness holder is still expected to be alive.
    cleanup
        .groups
        .retain(|pgid| leak_liveness_pipe && *pgid == fields["holder"]);
}

fn parse_pid_fields(line: &str) -> std::collections::BTreeMap<&str, i32> {
    line.split_whitespace()
        .filter_map(|field| field.split_once('='))
        .map(|(key, value)| {
            (
                key,
                value
                    .parse::<i32>()
                    .unwrap_or_else(|_| panic!("invalid pid field {key}={value}")),
            )
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn wait_for_native_exit(pid: i32) {
    let pid_fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) as i32 };
    if pid_fd < 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        return;
    }
    let mut poll_fd = libc::pollfd {
        fd: pid_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { libc::poll(&raw mut poll_fd, 1, 10_000) }, 1);
    unsafe {
        libc::close(pid_fd);
    }
}

#[cfg(target_os = "macos")]
fn wait_for_native_exit(pid: i32) {
    let queue = unsafe { libc::kqueue() };
    assert!(queue >= 0, "create process kqueue");
    let change = libc::kevent {
        ident: pid as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let timeout = libc::timespec {
        tv_sec: 10,
        tv_nsec: 0,
    };
    let mut event = change;
    let result = unsafe {
        libc::kevent(
            queue,
            &raw const change,
            1,
            &raw mut event,
            1,
            &raw const timeout,
        )
    };
    unsafe {
        libc::close(queue);
    }
    if result < 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    } else {
        assert_eq!(result, 1, "worker did not exit before kernel deadline");
    }
}
