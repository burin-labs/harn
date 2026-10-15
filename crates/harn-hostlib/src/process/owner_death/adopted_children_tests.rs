//! A failed bounded cleanup retains custody until actual native reaping.
use std::io::{BufRead, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, Stdio};

const ROLE: &str = "HARN_TEST_ADOPTION_HOLD_ROLE";
const FIXTURE: &str = "process::owner_death::adopted_children_tests::fixture";

fn command(role: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", FIXTURE, "--nocapture"])
        .env(ROLE, role);
    command
}

fn read_report(reader: &mut impl BufRead) -> serde_json::Value {
    loop {
        let mut line = String::new();
        assert_ne!(
            reader.read_line(&mut line).unwrap(),
            0,
            "fixture barrier missing"
        );
        if line.starts_with('{') {
            return serde_json::from_str(&line).unwrap();
        }
    }
}

fn report(value: serde_json::Value) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}

#[test]
fn fixture() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    if role == "leaf" {
        report(serde_json::json!({"child": std::process::id()}));
        loop {
            unsafe {
                libc::pause();
            }
        }
    }
    if role == "payload" {
        let mut cleanup = Cleanup {
            root: command("leaf").stdout(Stdio::piped()).spawn().unwrap(),
            adoptee: None,
        };
        let ready = read_report(&mut std::io::BufReader::new(
            cleanup.root.stdout.take().unwrap(),
        ));
        assert_eq!(ready["child"], cleanup.root.id());
        report(ready);
        // The external controller must bind the leaf before this guard lets
        // it outlive its payload. An earlier failure closes stdin instead.
        std::io::stdin().read_exact(&mut [0]).unwrap();
        std::mem::forget(cleanup);
        return;
    }
    assert_eq!(role, "hold");
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
        0
    );
    let mut payload = command("payload")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = std::io::BufReader::new(payload.stdout.take().unwrap());
    let ready = read_report(&mut output);
    report(serde_json::json!({"guardian": std::process::id(),
        "tid": unsafe {libc::syscall(libc::SYS_gettid)}, "child": ready["child"]}));
    std::io::stdin().read_exact(&mut [0]).unwrap();
    payload.stdin.take().unwrap().write_all(b"owned").unwrap();
    // Keep the reader alive while libtest writes its terminal summary.
    assert!(payload.wait().unwrap().success());
    drop(output);
    let token = harn_vm::op_interrupt::new_process_cleanup_token();
    harn_vm::op_interrupt::initialize_process_owner_group_journal(&token).unwrap();
    let journal = super::OwnerJournalCleanup(token.clone());
    let error = super::adopted_children::terminate_and_reap(std::time::Duration::ZERO).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(
        harn_vm::op_interrupt::initialize_process_owner_group_journal(&token)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::AlreadyExists
    );
    report(serde_json::json!({"journal_present": true}));
    let error = super::retain_adopted_children(error).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    let mut event = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    assert_eq!(
        unsafe {
            libc::waitid(
                libc::P_ALL,
                0,
                &raw mut event,
                libc::WEXITED | libc::WNOHANG,
            )
        },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    drop(journal);
    harn_vm::op_interrupt::initialize_process_owner_group_journal(&token).unwrap();
    harn_vm::op_interrupt::remove_process_owner_group_journal(&token);
    std::process::exit(1);
}

struct Cleanup {
    root: std::process::Child,
    adoptee: Option<OwnedFd>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(fd) = &self.adoptee {
            signal(fd);
        }
        let _ = self.root.kill();
        let _ = self.root.wait();
    }
}

fn signal(fd: &OwnedFd) {
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

#[test]
fn incomplete_cleanup_retains_custody_until_the_child_is_reaped() {
    let guardian = command("hold")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut cleanup = Cleanup {
        root: guardian,
        adoptee: None,
    };
    let errors = cleanup.root.stderr.take().unwrap();
    let (stderr_tx, stderr_rx) = std::sync::mpsc::channel();
    harn_parser::runtime_stack::spawn(move || {
        let mut text = String::new();
        for line in std::io::BufReader::new(errors).lines() {
            let line = line.unwrap();
            eprintln!("{line}");
            text.push_str(&line);
            text.push('\n');
        }
        stderr_tx.send(text).unwrap();
    });
    let mut stdout = std::io::BufReader::new(cleanup.root.stdout.take().unwrap());
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    harn_parser::runtime_stack::spawn(move || {
        ready_tx.send(read_report(&mut stdout)).unwrap();
        ready_tx.send(read_report(&mut stdout)).unwrap();
    });
    let ready = harn_clock::test_support::recv_within("fixture ownership handoff", &ready_rx);
    let guardian = ready["guardian"].as_u64().unwrap();
    assert_eq!(guardian, cleanup.root.id() as u64);
    let child = ready["child"].as_i64().unwrap() as i32;
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child, 0) };
    assert!(fd >= 0);
    cleanup.adoptee = Some(unsafe { OwnedFd::from_raw_fd(fd as i32) });
    cleanup
        .root
        .stdin
        .take()
        .unwrap()
        .write_all(b"owned")
        .unwrap();
    let checked =
        harn_clock::test_support::recv_within("actual guardian cleanup failure", &ready_rx);
    assert_eq!(checked["journal_present"], true);
    let tid = ready["tid"].as_i64().unwrap();
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(harn_clock::test_support::within(
            "guardian retains custody in native waitpid",
            async {
                loop {
                    assert!(cleanup.root.try_wait().unwrap().is_none());
                    let state =
                        std::fs::read_to_string(format!("/proc/{guardian}/task/{tid}/stat"))
                            .unwrap();
                    let syscall =
                        std::fs::read_to_string(format!("/proc/{guardian}/task/{tid}/syscall"))
                            .unwrap();
                    if state
                        .rsplit_once(") ")
                        .is_some_and(|(_, tail)| tail.starts_with("S "))
                        && syscall
                            .split_whitespace()
                            .next()
                            .and_then(|value| value.parse::<libc::c_long>().ok())
                            == Some(libc::SYS_wait4)
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            },
        ));
    let fd = cleanup.adoptee.as_ref().unwrap();
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(
        unsafe { libc::poll(&raw mut poll, 1, 0) },
        0,
        "custodied child is still alive"
    );
    signal(fd);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    harn_parser::runtime_stack::spawn(move || {
        let status = cleanup.root.wait().unwrap();
        done_tx.send(status.code()).unwrap();
    });
    let code = harn_clock::test_support::recv_within(
        "failed guardian terminal after real cleanup",
        &done_rx,
    );
    let stderr = harn_clock::test_support::recv_within("fixture stderr terminal", &stderr_rx);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("retaining child custody"));
}
