#![allow(dead_code)]

use std::fs;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

pub const PROCESS_FAIL_FAST_TIMEOUT: Duration = Duration::from_secs(60);
pub const EVENT_FAIL_FAST_TIMEOUT: Duration = Duration::from_secs(10);
pub const LOG_RECV_POLL_INTERVAL: Duration = Duration::from_millis(25);
pub const READY_PROBE_IO_TIMEOUT: Duration = Duration::from_millis(250);
pub const RETRY_POLL_INTERVAL: Duration = Duration::from_millis(25);
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

    pub fn terminate(&mut self) {
        if self
            .try_status()
            .unwrap_or_else(|error| panic!("{error}"))
            .is_some()
        {
            return;
        }
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.pid.to_string())
            .status()
            .unwrap();
        if !status.success()
            && self
                .try_status()
                .unwrap_or_else(|error| panic!("{error}"))
                .is_none()
        {
            panic!("kill exited with {status}");
        }
    }

    pub fn kill(&mut self) {
        if self
            .try_status()
            .unwrap_or_else(|error| panic!("{error}"))
            .is_some()
        {
            return;
        }
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg(self.pid.to_string())
            .status();
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

pub fn wait_for_existing_path(path: &Path, timeout: Duration) {
    wait_for_path_ready(path, timeout, PathReady::Exists)
}

pub fn wait_for_nonempty_file(path: &Path, timeout: Duration) {
    wait_for_path_ready(path, timeout, PathReady::NonEmptyFile)
}

enum PathReady {
    Exists,
    NonEmptyFile,
}

fn wait_for_path_ready(path: &Path, timeout: Duration, ready: PathReady) {
    if is_path_ready(path, &ready) {
        return;
    }

    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        panic!("watch parent directory missing: {}", parent.display());
    }

    path.file_name()
        .unwrap_or_else(|| panic!("wait_for_path requires a file name: {}", path.display()));

    let (tx, rx) = mpsc::channel::<()>();
    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |event: Result<Event, notify::Error>| {
            if event.is_ok() {
                let _ = tx.send(());
            }
        })
        .unwrap_or_else(|error| panic!("failed to install notify watcher: {error}"));
    watcher
        .watch(parent, RecursiveMode::NonRecursive)
        .unwrap_or_else(|error| panic!("failed to watch {}: {error}", parent.display()));

    let deadline = Instant::now() + timeout;
    loop {
        if is_path_ready(path, &ready) {
            return;
        }
        let remaining = match deadline.checked_duration_since(Instant::now()) {
            Some(remaining) => remaining,
            None => break,
        };
        match rx.recv_timeout(remaining) {
            Ok(()) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "notify watcher disconnected while waiting for {}",
                    path.display()
                );
            }
        }
    }
    panic!("timed out waiting for {}", path.display());
}

fn is_path_ready(path: &Path, ready: &PathReady) -> bool {
    match ready {
        PathReady::Exists => path.exists(),
        PathReady::NonEmptyFile => nonempty_file(path),
    }
}

fn nonempty_file(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false)
}

pub fn sleep_blocking(duration: Duration) {
    thread::sleep(duration);
}

pub async fn sleep_async(duration: Duration) {
    tokio::time::sleep(duration).await;
}
