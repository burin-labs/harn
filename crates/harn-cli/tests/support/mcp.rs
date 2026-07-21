#[path = "process.rs"]
mod process;

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value as JsonValue;

pub use process::HarnProcessTestNoLock;

#[allow(dead_code)]
pub fn lock_mcp_process_tests() -> HarnProcessTestNoLock {
    // The cross-process lock that this used to acquire was retired in favor
    // of tempdir + ephemeral-port isolation; see `support::process` for the
    // full rationale. Returning the unit sentinel keeps existing call sites
    // compiling and ergonomically correct (`let _lock = ...;`).
    process::lock_harn_process_tests()
}

/// Spawn a background thread that reads `reader` line by line, forwarding
/// each line over the returned channel and accumulating the full stream so
/// the joined handle yields everything that was read.
///
/// Used for both child stdout and stderr: the caller drains the channel
/// with a deadline for liveness and joins the handle (after the pipe
/// closes) for the complete transcript.
pub fn spawn_line_reader(
    reader: impl Read + Send + 'static,
) -> (Receiver<String>, JoinHandle<String>) {
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut collected = String::new();
        for line in BufReader::new(reader).lines() {
            let line = line.expect("child pipe line");
            collected.push_str(&line);
            collected.push('\n');
            let _ = tx.send(line);
        }
        collected
    });
    (rx, handle)
}

pub fn wait_for_child_log_suffix(
    child: &mut Child,
    rx: &Receiver<String>,
    needle: &str,
    timeout: Duration,
    label: &str,
) -> String {
    let deadline = Instant::now() + timeout;
    let mut observed = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok(line) if line.contains(needle) => {
                return line
                    .split(needle)
                    .nth(1)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
            }
            Ok(line) => observed.push(line),
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "{label} stderr stream closed before readiness log `{needle}` appeared\nstderr={}",
                    observed.join("\n")
                );
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    panic!(
        "timed out waiting for {label} readiness log `{needle}`\nstderr={}",
        observed.join("\n")
    );
}

/// Default per-operation deadline for the stdio MCP client.
///
/// The cold debug `harn` binary can take 30–40s to start its first request
/// under full nextest load (see `harn_serve_mcp_cli::PROCESS_READY_TIMEOUT`),
/// so the budget is generous — but it is *bounded*. Its whole purpose is to
/// keep a genuinely wedged server from consuming the nextest slow-test cap
/// (`terminate-after` at 180s) as an opaque timeout: on expiry the client
/// kills the child and panics with the captured stderr and the in-flight
/// request, turning a mystery hang into an actionable failure (harn#5397).
pub const STDIO_CLIENT_TIMEOUT: Duration = Duration::from_secs(60);

/// Bounded, self-diagnosing JSON-RPC-over-stdio client for the
/// `harn mcp serve` binary surface.
///
/// Every read is deadline-bounded and every failure path kills the child
/// and panics with the collected stderr plus the last request sent. This is
/// the deterministic counterpart to the HTTP path's
/// [`wait_for_child_log_suffix`]: no stdio test can silently hang to the
/// nextest cap, and any real server-side wedge surfaces its stderr for
/// diagnosis in the very CI run that hit it.
pub struct StdioMcpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout_rx: Receiver<String>,
    stdout_handle: Option<JoinHandle<String>>,
    stderr_rx: Receiver<String>,
    stderr_handle: Option<JoinHandle<String>>,
    timeout: Duration,
    last_request: Option<String>,
}

impl StdioMcpClient {
    /// Spawn `command` (already configured with the `mcp serve` arguments)
    /// with piped stdin/stdout and captured stderr.
    pub fn spawn(mut command: std::process::Command) -> Self {
        use std::process::Stdio;
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn `harn mcp serve`");
        let stdin = child.stdin.take().expect("mcp serve stdin piped");
        let (stdout_rx, stdout_handle) =
            spawn_line_reader(child.stdout.take().expect("mcp serve stdout piped"));
        let (stderr_rx, stderr_handle) =
            spawn_line_reader(child.stderr.take().expect("mcp serve stderr piped"));
        Self {
            child,
            stdin: Some(stdin),
            stdout_rx,
            stdout_handle: Some(stdout_handle),
            stderr_rx,
            stderr_handle: Some(stderr_handle),
            timeout: STDIO_CLIENT_TIMEOUT,
            last_request: None,
        }
    }

    /// Override the per-operation deadline. Primarily for exercising the
    /// diagnosis path itself without waiting the full default budget.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Send one JSON-RPC message, then read stdout lines until one whose
    /// `id` matches the request. Notifications and unrelated responses are
    /// skipped, mirroring a real client. Bounded by [`Self::timeout`].
    pub fn request(&mut self, request: JsonValue) -> JsonValue {
        let expected_id = request.get("id").cloned();
        self.send(&request);
        loop {
            let message = self.recv();
            let matches = expected_id
                .as_ref()
                .is_none_or(|id| message.get("id") == Some(id));
            if matches {
                return message;
            }
        }
    }

    /// Read the next JSON message from stdout, skipping any for which `keep`
    /// is false and handing each skipped message to `observe` first (used to
    /// collect progress notifications). Bounded by [`Self::timeout`].
    pub fn recv_until(
        &mut self,
        mut observe: impl FnMut(&JsonValue),
        mut keep: impl FnMut(&JsonValue) -> bool,
    ) -> JsonValue {
        loop {
            let message = self.recv();
            if keep(&message) {
                return message;
            }
            observe(&message);
        }
    }

    /// Write one JSON-RPC message to the child's stdin.
    pub fn send(&mut self, message: &JsonValue) {
        let encoded = serde_json::to_string(message).expect("serialize JSON-RPC message");
        self.last_request = Some(encoded.clone());
        let stdin = self.stdin.as_mut().expect("stdin still open");
        if writeln!(stdin, "{encoded}")
            .and_then(|()| stdin.flush())
            .is_err()
        {
            self.diagnose("failed to write request to `harn mcp serve` stdin");
        }
    }

    fn recv(&mut self) -> JsonValue {
        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.diagnose(&format!(
                    "timed out after {:?} waiting for an MCP stdout response",
                    self.timeout
                ));
            }
            match self.stdout_rx.recv_timeout(remaining) {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => match serde_json::from_str::<JsonValue>(line.trim()) {
                    Ok(value) => return value,
                    Err(error) => {
                        self.diagnose(&format!("non-JSON line on MCP stdout ({error}): {line:?}"))
                    }
                },
                Err(RecvTimeoutError::Timeout) => self.diagnose(&format!(
                    "timed out after {:?} waiting for an MCP stdout response",
                    self.timeout
                )),
                Err(RecvTimeoutError::Disconnected) => {
                    self.diagnose("MCP server closed stdout before responding")
                }
            }
        }
    }

    /// Drop stdin (signalling EOF) and wait, bounded, for a successful exit.
    /// A server that does not exit promptly on EOF is diagnosed with its
    /// stderr rather than left to consume the nextest slow-test cap.
    pub fn shutdown_expect_success(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + self.timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    assert!(
                        status.success(),
                        "`harn mcp serve` exited unsuccessfully: {status}\nstderr:\n{}",
                        self.collect_stderr()
                    );
                    return;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        self.diagnose(
                            "`harn mcp serve` did not exit within the deadline after stdin EOF",
                        );
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => panic!("waiting on `harn mcp serve` failed: {error}"),
            }
        }
    }

    /// Kill the child and panic with `context`, the last request sent, and
    /// the full captured stderr. Never returns.
    fn diagnose(&mut self, context: &str) -> ! {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let last_request = self
            .last_request
            .clone()
            .unwrap_or_else(|| "<none>".to_string());
        let stderr = self.collect_stderr();
        panic!("{context}\nlast request: {last_request}\n`harn mcp serve` stderr:\n{stderr}");
    }

    /// Join the stderr reader (the pipe has closed once the child exited or
    /// was killed) to recover the complete stderr transcript, falling back
    /// to draining the channel if the handle was already taken.
    fn collect_stderr(&mut self) -> String {
        if let Some(handle) = self.stderr_handle.take() {
            if let Ok(collected) = handle.join() {
                return collected;
            }
        }
        let mut lines = Vec::new();
        while let Ok(line) = self.stderr_rx.recv_timeout(Duration::from_secs(2)) {
            lines.push(line);
        }
        lines.join("\n")
    }
}

impl Drop for StdioMcpClient {
    fn drop(&mut self) {
        // Guarantee the child never outlives the test even on an assertion
        // panic mid-conversation, and that the reader threads observe closed
        // pipes so they terminate rather than leak.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.stdout_handle.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.stderr_handle.take() {
            let _ = handle.join();
        }
    }
}
