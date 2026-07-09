//! Test-only [`ProcessSpawner`] / [`ProcessHandle`] implementations.
//!
//! Tests install a [`MockSpawner`] via
//! [`super::handle::install_spawner`], enqueue per-spawn responses, and
//! drive the resulting [`MockProcess`] state explicitly via the controller
//! returned at enqueue time. There are zero real subprocesses, no
//! `thread::sleep`, no `Instant::now` polling.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use super::handle::{
    ExitStatus, ProcessCleanupReport, ProcessError, ProcessHandle, ProcessKiller, ProcessSpawner,
    SpawnSpec, WaitOutcome,
};

/// Behaviour to script for a single mocked spawn.
#[derive(Clone, Debug)]
pub struct MockProcessConfig {
    /// PID returned by [`ProcessHandle::pid`] for this spawn. Must be > 0
    /// because `process_tools` test assertions check `> 0`.
    pub pid: u32,
    /// Process-group id returned by [`ProcessHandle::process_group_id`].
    pub pgid: Option<u32>,
    /// Initial stdout bytes available before any test-side appends.
    pub stdout: Vec<u8>,
    /// Initial stderr bytes available before any test-side appends.
    pub stderr: Vec<u8>,
    /// If `Some`, the process is already complete and `wait*` returns this
    /// immediately. If `None`, the process stays "running" until the test
    /// signals exit via the controller.
    pub exit_status: Option<ExitStatus>,
    /// If `true`, [`ProcessHandle::wait_with_timeout`] reports a timeout
    /// regardless of `exit_status`. Used to test the timeout path without
    /// real subprocess scheduling.
    pub force_timeout: bool,
    /// If non-`None`, force [`ProcessSpawner::spawn`] to fail with this
    /// error instead of returning a handle. Used to exercise sandbox /
    /// invalid-argv error paths.
    pub spawn_error: Option<ProcessError>,
    /// If non-`None`, force waits to fail with this I/O error.
    pub wait_error: Option<String>,
    /// Cleanup report returned when timeout/cancel paths kill this process.
    pub cleanup_report: Option<ProcessCleanupReport>,
    /// Keep stdout open after the direct child exits until the cleanup killer
    /// runs. This models an escaped descendant that inherited fd 1.
    pub stdout_hangs_after_exit_until_kill: bool,
    /// Keep stderr open after the direct child exits until the cleanup killer
    /// runs. This models an escaped descendant that inherited fd 2.
    pub stderr_hangs_after_exit_until_kill: bool,
}

impl Default for MockProcessConfig {
    fn default() -> Self {
        Self {
            pid: 99_999,
            pgid: Some(99_999),
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_status: Some(ExitStatus::from_code(0)),
            force_timeout: false,
            spawn_error: None,
            wait_error: None,
            cleanup_report: None,
            stdout_hangs_after_exit_until_kill: false,
            stderr_hangs_after_exit_until_kill: false,
        }
    }
}

impl MockProcessConfig {
    /// Convenience: build a successful spawn with the given exit code, no
    /// stdout/stderr.
    pub fn completed(exit_code: i32) -> Self {
        Self {
            exit_status: Some(ExitStatus::from_code(exit_code)),
            ..Self::default()
        }
    }

    /// Convenience: build a successful spawn with the given exit code and
    /// inline stdout payload.
    pub fn with_stdout(exit_code: i32, stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            stdout: stdout.into(),
            exit_status: Some(ExitStatus::from_code(exit_code)),
            ..Self::default()
        }
    }

    /// Convenience: build a config that stays "running" until the test
    /// signals exit via the controller. Used for long-running and
    /// timeout tests.
    pub fn running() -> Self {
        Self {
            exit_status: None,
            ..Self::default()
        }
    }
}

#[derive(Default)]
struct MockSpawnerInner {
    queue: VecDeque<(MockProcessConfig, Arc<MockState>)>,
    captured: Vec<SpawnSpec>,
    last_controller: Option<MockHandleController>,
}

/// Test [`ProcessSpawner`] that returns scripted [`MockProcess`] handles
/// and captures the [`SpawnSpec`] passed to each spawn.
pub struct MockSpawner {
    inner: Mutex<MockSpawnerInner>,
}

impl Default for MockSpawner {
    fn default() -> Self {
        Self::new()
    }
}

impl MockSpawner {
    /// Build an empty spawner. Call [`Self::enqueue`] to script behaviour
    /// for each anticipated spawn.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(MockSpawnerInner::default()),
        }
    }

    /// Enqueue a configuration for the next spawn. Returns a controller
    /// that lets the test drive the resulting [`MockProcess`] state
    /// (append stdout, complete with status, etc.). For one-shot
    /// foreground tests, the controller may simply be dropped.
    pub fn enqueue(&self, config: MockProcessConfig) -> MockHandleController {
        let state = Arc::new(MockState::new(&config));
        let controller = MockHandleController {
            state: Arc::clone(&state),
        };
        let mut inner = self.inner.lock().expect("MockSpawner mutex poisoned");
        inner.queue.push_back((config, state));
        inner.last_controller = Some(controller.clone());
        controller
    }

    /// Returns the [`SpawnSpec`] objects captured so far, in order.
    pub fn captured(&self) -> Vec<SpawnSpec> {
        self.inner
            .lock()
            .expect("MockSpawner mutex poisoned")
            .captured
            .clone()
    }

    /// Returns the latest controller installed via [`Self::enqueue`].
    /// Convenience for tests that only enqueue one config.
    pub fn last_controller(&self) -> Option<MockHandleController> {
        self.inner
            .lock()
            .expect("MockSpawner mutex poisoned")
            .last_controller
            .clone()
    }
}

impl ProcessSpawner for MockSpawner {
    fn spawn(&self, spec: SpawnSpec) -> Result<Box<dyn ProcessHandle>, ProcessError> {
        let (config, state) = {
            let mut inner = self.inner.lock().expect("MockSpawner mutex poisoned");
            inner.captured.push(spec);
            inner.queue.pop_front().expect(
                "MockSpawner: spawn() called with no enqueued configuration. Call \
                 MockSpawner::enqueue(...) before each expected spawn.",
            )
        };

        if let Some(err) = config.spawn_error {
            return Err(err);
        }

        let killer: Arc<dyn ProcessKiller> = Arc::new(MockKiller {
            pid: config.pid,
            state: Arc::clone(&state),
        });

        Ok(Box::new(MockProcess {
            pid: config.pid,
            pgid: config.pgid,
            killer,
            state,
            stdin_taken: false,
            stdout_taken: false,
            stderr_taken: false,
        }))
    }
}

/// Test-side controller for a [`MockProcess`]. Cloneable; all clones
/// reference the same underlying state.
#[derive(Clone)]
pub struct MockHandleController {
    state: Arc<MockState>,
}

impl MockHandleController {
    /// Append bytes to the mock's stdout buffer. Subsequent reads on the
    /// stdout reader will see them.
    pub fn append_stdout(&self, bytes: &[u8]) {
        let mut data = self.state.stdout.lock().unwrap();
        data.extend_from_slice(bytes);
        self.state.stdout_cv.notify_all();
    }

    /// Append bytes to the mock's stderr buffer.
    pub fn append_stderr(&self, bytes: &[u8]) {
        let mut data = self.state.stderr.lock().unwrap();
        data.extend_from_slice(bytes);
        self.state.stderr_cv.notify_all();
    }

    /// Mark the process as having exited with the given status. Drains
    /// any blocked `wait()` callers and closes the stdout/stderr readers.
    pub fn complete_with(&self, status: ExitStatus) {
        let mut exit = self.state.exit.lock().unwrap();
        if exit.is_none() {
            *exit = Some(ExitOutcome {
                status,
                killed: false,
            });
        }
        drop(exit);
        self.state.notify_exit_and_pipes();
    }

    /// Returns true if [`MockKiller::kill`] has been invoked since spawn.
    pub fn was_killed(&self) -> bool {
        self.state
            .exit
            .lock()
            .unwrap()
            .as_ref()
            .map(|o| o.killed)
            .unwrap_or(false)
    }

    /// Returns the bytes the test-tool side wrote to the mock's stdin
    /// reader (after the process-tool path closed stdin).
    pub fn stdin_written(&self) -> Vec<u8> {
        self.state.stdin_written.lock().unwrap().clone()
    }
}

struct MockState {
    /// Bytes available to the stdout reader. Drained as the reader pulls.
    stdout: Mutex<Vec<u8>>,
    /// Bytes available to the stderr reader.
    stderr: Mutex<Vec<u8>>,
    /// Captured stdin bytes the spawn-side wrote.
    stdin_written: Mutex<Vec<u8>>,
    /// Final status, set by `complete_with` or by the killer.
    exit: Mutex<Option<ExitOutcome>>,
    exit_cv: Condvar,
    stdout_cv: Condvar,
    stderr_cv: Condvar,
    /// Force-timeout config copied from MockProcessConfig.
    force_timeout: bool,
    wait_error: Option<String>,
    cleanup_report: Option<ProcessCleanupReport>,
    stdout_hangs_after_exit_until_kill: bool,
    stderr_hangs_after_exit_until_kill: bool,
    pipes_released_by_cleanup: Mutex<bool>,
}

#[derive(Clone, Copy, Debug)]
struct ExitOutcome {
    status: ExitStatus,
    killed: bool,
}

impl MockState {
    fn new(config: &MockProcessConfig) -> Self {
        let exit = config.exit_status.map(|status| ExitOutcome {
            status,
            killed: false,
        });
        Self {
            stdout: Mutex::new(config.stdout.clone()),
            stderr: Mutex::new(config.stderr.clone()),
            stdin_written: Mutex::new(Vec::new()),
            exit: Mutex::new(exit),
            exit_cv: Condvar::new(),
            stdout_cv: Condvar::new(),
            stderr_cv: Condvar::new(),
            force_timeout: config.force_timeout,
            wait_error: config.wait_error.clone(),
            cleanup_report: config.cleanup_report.clone(),
            stdout_hangs_after_exit_until_kill: config.stdout_hangs_after_exit_until_kill,
            stderr_hangs_after_exit_until_kill: config.stderr_hangs_after_exit_until_kill,
            pipes_released_by_cleanup: Mutex::new(false),
        }
    }

    fn is_exited(&self) -> bool {
        self.exit.lock().unwrap().is_some()
    }

    fn wait_for_exit(&self, timeout: Option<Duration>) -> Option<ExitOutcome> {
        let mut exit = self.exit.lock().unwrap();
        if let Some(timeout) = timeout {
            if exit.is_none() {
                let (next, result) = self.exit_cv.wait_timeout(exit, timeout).unwrap();
                exit = next;
                if result.timed_out() && exit.is_none() {
                    return None;
                }
            }
        } else {
            while exit.is_none() {
                exit = self.exit_cv.wait(exit).unwrap();
            }
        }
        *exit
    }

    fn record_kill(&self) {
        let mut exit = self.exit.lock().unwrap();
        if exit.is_none() {
            *exit = Some(ExitOutcome {
                status: ExitStatus::from_signal(9),
                killed: true,
            });
        } else if let Some(outcome) = exit.as_mut() {
            outcome.killed = true;
        }
        drop(exit);
        *self.pipes_released_by_cleanup.lock().unwrap() = true;
        self.notify_exit_and_pipes();
    }

    fn pipe_has_reached_eof(&self, kind: PipeKind) -> bool {
        if !self.is_exited() {
            return false;
        }
        let hangs_until_cleanup = match kind {
            PipeKind::Stdout => self.stdout_hangs_after_exit_until_kill,
            PipeKind::Stderr => self.stderr_hangs_after_exit_until_kill,
        };
        !hangs_until_cleanup || *self.pipes_released_by_cleanup.lock().unwrap()
    }

    fn notify_exit_and_pipes(&self) {
        self.exit_cv.notify_all();

        // Pipe readers wait on the pipe mutex but also observe `exit`. Take
        // the pipe locks before notifying so an exit cannot be signaled in the
        // gap between a reader's exit check and its condvar wait.
        {
            let _stdout = self.stdout.lock().unwrap();
            self.stdout_cv.notify_all();
        }
        {
            let _stderr = self.stderr.lock().unwrap();
            self.stderr_cv.notify_all();
        }
    }

    fn cleanup_report(&self, root_pid: u32, signal: i32) -> ProcessCleanupReport {
        self.cleanup_report
            .clone()
            .unwrap_or_else(|| ProcessCleanupReport::for_signal(Some(root_pid), signal))
    }
}

/// Mock process backed by a shared `MockState`.
pub struct MockProcess {
    pid: u32,
    pgid: Option<u32>,
    killer: Arc<dyn ProcessKiller>,
    state: Arc<MockState>,
    stdin_taken: bool,
    stdout_taken: bool,
    stderr_taken: bool,
}

impl ProcessHandle for MockProcess {
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn process_group_id(&self) -> Option<u32> {
        self.pgid
    }

    fn killer(&self) -> Arc<dyn ProcessKiller> {
        Arc::clone(&self.killer)
    }

    fn take_stdin(&mut self) -> Option<Box<dyn Write + Send>> {
        if self.stdin_taken {
            return None;
        }
        self.stdin_taken = true;
        Some(Box::new(MockStdin {
            state: Arc::clone(&self.state),
        }))
    }

    fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        if self.stdout_taken {
            return None;
        }
        self.stdout_taken = true;
        Some(Box::new(MockStdoutReader {
            state: Arc::clone(&self.state),
            kind: PipeKind::Stdout,
        }))
    }

    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        if self.stderr_taken {
            return None;
        }
        self.stderr_taken = true;
        Some(Box::new(MockStdoutReader {
            state: Arc::clone(&self.state),
            kind: PipeKind::Stderr,
        }))
    }

    fn wait_with_timeout(
        &mut self,
        timeout: Option<Duration>,
        interrupt: &dyn Fn() -> bool,
    ) -> io::Result<WaitOutcome> {
        if let Some(error) = self.state.wait_error.as_ref() {
            return Err(io::Error::other(error.clone()));
        }
        if self.state.force_timeout {
            self.state.record_kill();
            return Ok(WaitOutcome::TimedOut(
                self.state.cleanup_report(self.pid, 9),
            ));
        }
        // Wait in short condvar slices so the interrupt callback is observed
        // (mirrors the real spawner's ~20ms `try_wait` poll loop) while
        // remaining deterministic: nothing here depends on how many slices
        // elapse, only on which condition fires first.
        let deadline = timeout.map(|timeout| std::time::Instant::now() + timeout);
        loop {
            if let Some(outcome) = self.state.wait_for_exit(Some(Duration::from_millis(5))) {
                return Ok(WaitOutcome::Exited(outcome.status));
            }
            if interrupt() {
                self.state.record_kill();
                return Ok(WaitOutcome::Interrupted(
                    self.state.cleanup_report(self.pid, 15),
                ));
            }
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                self.state.record_kill();
                return Ok(WaitOutcome::TimedOut(
                    self.state.cleanup_report(self.pid, 9),
                ));
            }
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(error) = self.state.wait_error.as_ref() {
            return Err(io::Error::other(error.clone()));
        }
        let outcome = self
            .state
            .wait_for_exit(None)
            .expect("wait without timeout returned None");
        Ok(outcome.status)
    }
}

struct MockStdin {
    state: Arc<MockState>,
}

impl Write for MockStdin {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.state
            .stdin_written
            .lock()
            .unwrap()
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum PipeKind {
    Stdout,
    Stderr,
}

struct MockStdoutReader {
    state: Arc<MockState>,
    kind: PipeKind,
}

impl MockStdoutReader {
    fn pipe_lock(&self) -> &Mutex<Vec<u8>> {
        match self.kind {
            PipeKind::Stdout => &self.state.stdout,
            PipeKind::Stderr => &self.state.stderr,
        }
    }

    fn pipe_cv(&self) -> &Condvar {
        match self.kind {
            PipeKind::Stdout => &self.state.stdout_cv,
            PipeKind::Stderr => &self.state.stderr_cv,
        }
    }
}

impl Read for MockStdoutReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let lock = self.pipe_lock();
        let cv = self.pipe_cv();
        let mut data = lock.lock().unwrap();
        loop {
            if !data.is_empty() {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                data.drain(..n);
                return Ok(n);
            }
            // Empty buffer: if the process is exited, signal EOF;
            // otherwise wait for either more bytes or exit.
            if self.state.pipe_has_reached_eof(self.kind) {
                return Ok(0);
            }
            data = cv.wait(data).unwrap();
        }
    }
}

struct MockKiller {
    pid: u32,
    state: Arc<MockState>,
}

impl ProcessKiller for MockKiller {
    fn kill(&self) -> ProcessCleanupReport {
        self.state.record_kill();
        self.state.cleanup_report(self.pid, 9)
    }
}
