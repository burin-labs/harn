#![allow(dead_code)]

use std::process::{Child, Command, ExitStatus};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

pub const SLACK_ACK_TIMEOUT: Duration = Duration::from_secs(3);

pub struct ChildExitWatcher {
    pid: u32,
    rx: Receiver<Result<ExitStatus, String>>,
    status: Option<Result<ExitStatus, String>>,
    wait_thread: Option<thread::JoinHandle<()>>,
}

impl ChildExitWatcher {
    pub fn new(mut child: Child) -> Self {
        let pid = child.id();
        let (tx, rx) = mpsc::channel();
        let wait_thread = thread::spawn(move || {
            let result = child.wait().map_err(|error| error.to_string());
            let _ = tx.send(result);
        });
        Self {
            pid,
            rx,
            status: None,
            wait_thread: Some(wait_thread),
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn try_status(&mut self) -> Result<Option<ExitStatus>, String> {
        if let Some(status) = &self.status {
            return status
                .as_ref()
                .map(|status| Some(*status))
                .map_err(Clone::clone);
        }
        match self.rx.try_recv() {
            Ok(status) => self.cache_status(status),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("process wait thread disconnected before reporting exit".to_string())
            }
        }
    }

    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<ExitStatus, String> {
        if let Some(status) = &self.status {
            return status.as_ref().copied().map_err(Clone::clone);
        }
        match self.rx.recv_timeout(timeout) {
            Ok(status) => match self.cache_status(status)? {
                Some(status) => Ok(status),
                None => unreachable!("cache_status returns Some after receiving a status"),
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err(format!("timed out waiting for process {} exit", self.pid))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("process wait thread disconnected before reporting exit".to_string())
            }
        }
    }

    pub fn wait_for_success(&mut self, timeout: Duration) {
        let status = self
            .wait_timeout(timeout)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(status.success(), "child exited unsuccessfully: {status}");
    }

    pub fn wait_for_code(&mut self, timeout: Duration, expected: i32) {
        let status = self
            .wait_timeout(timeout)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            status.code(),
            Some(expected),
            "unexpected exit status: {status}"
        );
    }

    /// Request graceful shutdown of the child.
    ///
    /// On Unix this delivers SIGTERM and the child runs its drain. Windows
    /// has no portable equivalent that reaches an arbitrary console child
    /// without taking over the parent's console group, so the Windows path
    /// falls back to a forceful `taskkill /F` — no graceful drain, no
    /// shutdown-needle log line. Tests that assert on graceful-shutdown
    /// semantics must therefore stay `#![cfg(unix)]`.
    pub fn terminate(&mut self) {
        if self
            .try_status()
            .unwrap_or_else(|error| panic!("{error}"))
            .is_some()
        {
            return;
        }
        let status = posix_kill_or_taskkill(self.pid, KillKind::Term);
        if !status.success()
            && self
                .try_status()
                .unwrap_or_else(|error| panic!("{error}"))
                .is_none()
        {
            panic!("kill exited with {status}");
        }
    }

    /// Forcefully terminate the child.
    ///
    /// Maps to SIGKILL on Unix and `taskkill /F` on Windows (which calls
    /// `TerminateProcess` underneath). Both paths are immediate and skip
    /// any drain logic.
    pub fn kill(&mut self) {
        if self
            .try_status()
            .unwrap_or_else(|error| panic!("{error}"))
            .is_some()
        {
            return;
        }
        let _ = posix_kill_or_taskkill(self.pid, KillKind::Kill);
    }

    fn cache_status(
        &mut self,
        status: Result<ExitStatus, String>,
    ) -> Result<Option<ExitStatus>, String> {
        self.join_wait_thread();
        let result = status.as_ref().copied().map(Some).map_err(Clone::clone);
        self.status = Some(status);
        result
    }

    fn join_wait_thread(&mut self) {
        if let Some(wait_thread) = self.wait_thread.take() {
            wait_thread.join().expect("process wait thread");
        }
    }
}

pub fn sleep_blocking(duration: Duration) {
    thread::sleep(duration);
}

#[derive(Copy, Clone)]
enum KillKind {
    Term,
    Kill,
}

#[cfg(unix)]
fn posix_kill_or_taskkill(pid: u32, kind: KillKind) -> ExitStatus {
    let signal = match kind {
        KillKind::Term => "-TERM",
        KillKind::Kill => "-KILL",
    };
    Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .status()
        .unwrap()
}

#[cfg(windows)]
fn posix_kill_or_taskkill(pid: u32, _kind: KillKind) -> ExitStatus {
    // Windows has no portable signal-delivery mechanism for arbitrary console
    // children, so both Term and Kill collapse to a forceful TerminateProcess
    // via `taskkill /F`. The orchestrator's drain logic is therefore not
    // exercised on this platform; tests that depend on it must remain gated
    // to `#![cfg(unix)]`.
    Command::new("taskkill")
        .arg("/F")
        .arg("/PID")
        .arg(pid.to_string())
        .status()
        .unwrap()
}
