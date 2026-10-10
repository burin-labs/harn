//! Ready child processes for interrupt and escalation probes.

use super::*;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::IntoRawFd;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicI32;

static MARKER_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn handle_term(_: libc::c_int) {
    // The handler can run on libtest's driver thread. Use only
    // async-signal-safe syscalls, with an already-open marker file.
    unsafe {
        libc::write(
            MARKER_FD.load(Ordering::Relaxed),
            b"TERM".as_ptr().cast(),
            4,
        );
        libc::_exit(0);
    }
}

#[test]
fn process_probe_child() {
    let Ok(mode) = std::env::var("HARN_INTERRUPT_PROBE_MODE") else {
        return;
    };
    let mut leaf = None;
    match mode.as_str() {
        "immune" => unsafe {
            assert_ne!(libc::signal(libc::SIGTERM, libc::SIG_IGN), libc::SIG_ERR);
        },
        "polite" => {
            let marker =
                std::fs::File::create(std::env::var_os("HARN_INTERRUPT_PROBE_MARKER").unwrap())
                    .expect("open SIGTERM marker");
            MARKER_FD.store(marker.into_raw_fd(), Ordering::Relaxed);
            unsafe {
                assert_ne!(
                    libc::signal(
                        libc::SIGTERM,
                        handle_term as *const () as libc::sighandler_t
                    ),
                    libc::SIG_ERR
                );
            }
        }
        "tree" => {
            // The descendant inherits the parent's process group and must
            // report readiness before the parent reports its own readiness.
            leaf = Some(start_process_probe("leaf", false, None));
        }
        "leaf" => {}
        other => panic!("unknown process probe mode: {other}"),
    }
    println!("\nprobe-ready");
    std::io::stdout().flush().expect("flush probe readiness");
    // Keep the descendant's cleanup guard alive until the group is killed.
    let _leaf = leaf;
    loop {
        std::thread::park();
    }
}

pub(super) struct ProbeChild {
    child: Child,
    group: bool,
}

impl std::ops::Deref for ProbeChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.child
    }
}

impl std::ops::DerefMut for ProbeChild {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for ProbeChild {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        if self.group {
            terminate_child_group(&mut self.child);
        } else {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub(super) fn start_process_probe(
    mode: &str,
    group: bool,
    marker: Option<&std::path::Path>,
) -> ProbeChild {
    let mut command = Command::new(std::env::current_exe().expect("test binary"));
    command
        .args([
            "--exact",
            "op_interrupt::probe_tests::process_probe_child",
            "--nocapture",
        ])
        .env("HARN_INTERRUPT_PROBE_MODE", mode)
        .env_remove("HARN_INTERRUPT_PROBE_MARKER")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(marker) = marker {
        command.env("HARN_INTERRUPT_PROBE_MARKER", marker);
    }
    if group {
        configure_kill_group(&mut command);
    }
    let mut child = ProbeChild {
        child: command.spawn().expect("spawn process probe"),
        group,
    };
    let stdout = child.stdout.take().expect("probe stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let ready = BufReader::new(stdout)
            .lines()
            .any(|line| line.is_ok_and(|line| line == "probe-ready"));
        let _ = tx.send(ready);
    });
    assert!(
        harn_clock::test_support::recv_within("process probe readiness", &rx),
        "process probe exited before readiness"
    );
    child
}
