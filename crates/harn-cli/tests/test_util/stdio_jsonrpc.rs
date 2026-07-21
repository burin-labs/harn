//! Bounded, self-diagnosing JSON-RPC-over-stdio client for the interactive
//! `harn` server surfaces (`mcp serve`, `serve acp`, `serve test`).
//!
//! Every one of these suites drives a long-lived child over piped stdin/stdout
//! and used to read each response with an **unbounded** `read_line`, capture no
//! stderr (`Stdio::null`/`inherit`), and reap with an unbounded `child.wait()`.
//! A server that wedged mid-conversation therefore consumed the whole nextest
//! slow-test cap (`terminate-after` at 180s) as an opaque `TIMEOUT` with no
//! signal — the exact flake fixed for `mcp serve` in harn#5397/#5398 and now
//! prevented for the whole class in harn#5401.
//!
//! This is the one owner of that safety net: every read is deadline-bounded and
//! every failure path kills the child and panics with the captured stderr plus
//! the last request sent, so a wedge surfaces an actionable diagnostic in the
//! very CI run that hit it instead of a 180s mystery. It is the deterministic
//! counterpart to the HTTP path's [`wait_for_child_log_suffix`].
//!
//! `#[allow(dead_code)]`: this module is compiled into several test binaries
//! and no single one drives every surface, so the unused-in-that-binary items
//! would otherwise trip `-D warnings`.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value as JsonValue};

/// Default per-operation deadline for the stdio JSON-RPC client.
///
/// The cold debug `harn` binary can take 30–40s to start its first request
/// under full nextest load (see `harn_serve_mcp_cli::PROCESS_READY_TIMEOUT`),
/// so the budget is generous — but it is *bounded*. Its whole purpose is to
/// keep a genuinely wedged server from consuming the nextest slow-test cap as
/// an opaque timeout: on expiry the client kills the child and panics with the
/// captured stderr and the in-flight request, turning a mystery hang into an
/// actionable failure (harn#5397).
#[allow(dead_code)]
pub const STDIO_CLIENT_TIMEOUT: Duration = Duration::from_mins(1);

/// Spawn a background thread that reads `reader` line by line, forwarding each
/// line over the returned channel and accumulating the full stream so the
/// joined handle yields everything that was read.
///
/// Used for both child stdout and stderr: the caller drains the channel with a
/// deadline for liveness and joins the handle (after the pipe closes) for the
/// complete transcript.
#[allow(dead_code)]
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

/// Block, bounded, until a line containing `needle` appears on `rx`, returning
/// the trimmed suffix after `needle`. Used by the HTTP transport smoke to learn
/// the ephemeral listener address from the child's stderr readiness log. On
/// timeout or premature stream close the child is killed and the panic carries
/// everything observed so far.
#[allow(dead_code)]
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
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => {
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

/// The outcome of a request/response [`exchange`](StdioJsonRpcClient::exchange):
/// the matching response plus every notification (and any other non-matching
/// message) observed while waiting for it, in arrival order.
#[allow(dead_code)]
pub struct Exchange {
    pub notifications: Vec<JsonValue>,
    pub response: JsonValue,
}

/// Bounded, self-diagnosing JSON-RPC-over-stdio client. See the module docs.
///
/// `surface` names the child binary (e.g. `"harn mcp serve"`) so every
/// diagnostic points at the process that actually wedged.
#[allow(dead_code)]
pub struct StdioJsonRpcClient {
    surface: &'static str,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout_rx: Receiver<String>,
    stdout_handle: Option<JoinHandle<String>>,
    stderr_rx: Receiver<String>,
    stderr_handle: Option<JoinHandle<String>>,
    timeout: Duration,
    last_request: Option<String>,
}

#[allow(dead_code)]
impl StdioJsonRpcClient {
    /// Spawn `command` (already configured with the server's subcommand and
    /// arguments) with piped stdin/stdout and captured stderr. `surface` is the
    /// human name of the child used in diagnostics.
    pub fn spawn(surface: &'static str, mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("spawn `{surface}`: {error}"));
        let stdin = child.stdin.take().expect("child stdin piped");
        let (stdout_rx, stdout_handle) =
            spawn_line_reader(child.stdout.take().expect("child stdout piped"));
        let (stderr_rx, stderr_handle) =
            spawn_line_reader(child.stderr.take().expect("child stderr piped"));
        Self {
            surface,
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

    /// Send one JSON-RPC message, then read until the response whose `id`
    /// matches, discarding notifications. For surfaces that never issue
    /// server→client requests (MCP); a stray one panics. Bounded by the
    /// per-operation deadline.
    pub fn request(&mut self, request: JsonValue) -> JsonValue {
        let surface = self.surface;
        self.exchange(request, |method| {
            panic!("unexpected server→client request `{method}` on `{surface}`")
        })
        .response
    }

    /// Send `request` and read until the response whose `id` matches, answering
    /// any server→client request (a message carrying both `method` and `id`)
    /// through `on_server_request` — which returns the JSON `result` to reply
    /// with — and collecting every other message as a notification. This is the
    /// bidirectional pattern the ACP agent surface needs. Bounded by the
    /// per-operation deadline.
    pub fn exchange(
        &mut self,
        request: JsonValue,
        mut on_server_request: impl FnMut(&str) -> JsonValue,
    ) -> Exchange {
        let request_id = request.get("id").cloned();
        self.send(&request);
        let mut notifications = Vec::new();
        loop {
            let message = self.recv();
            if message.get("method").is_some() && message.get("id").is_some() {
                let method = message["method"].as_str().unwrap_or_default().to_string();
                let result = on_server_request(&method);
                self.send(&json!({
                    "jsonrpc": "2.0",
                    "id": message["id"].clone(),
                    "result": result,
                }));
                continue;
            }
            if request_id
                .as_ref()
                .is_some_and(|id| message.get("id") == Some(id))
            {
                return Exchange {
                    notifications,
                    response: message,
                };
            }
            notifications.push(message);
        }
    }

    /// Read stdout messages until one for which `keep` is true, handing each
    /// skipped message to `observe` first (used to collect progress
    /// notifications). Bounded by the per-operation deadline.
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
            self.diagnose("failed to write request to child stdin");
        }
    }

    /// Read and parse the next JSON message from stdout, skipping blank lines.
    /// Bounded by the per-operation deadline; a wedge is diagnosed, never
    /// blocked on.
    pub fn recv(&mut self) -> JsonValue {
        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.diagnose(&format!(
                    "timed out after {:?} waiting for a stdout response",
                    self.timeout
                ));
            }
            match self.stdout_rx.recv_timeout(remaining) {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => match serde_json::from_str::<JsonValue>(line.trim()) {
                    Ok(value) => return value,
                    Err(error) => {
                        self.diagnose(&format!("non-JSON line on stdout ({error}): {line:?}"))
                    }
                },
                Err(RecvTimeoutError::Timeout) => self.diagnose(&format!(
                    "timed out after {:?} waiting for a stdout response",
                    self.timeout
                )),
                Err(RecvTimeoutError::Disconnected) => {
                    self.diagnose("server closed stdout before responding")
                }
            }
        }
    }

    /// Drop stdin (signalling EOF) and wait, bounded, for a successful exit.
    /// A server that does not exit promptly on EOF is diagnosed with its
    /// stderr rather than left to consume the nextest slow-test cap.
    ///
    /// The wait is event-driven, not polled: the stdout reader thread's channel
    /// disconnects when the child closes stdout, which for these servers
    /// happens as they exit on EOF. Blocking on that channel with a deadline
    /// (rather than a `try_wait` + sleep loop) keeps the bound without a
    /// wall-clock poll.
    pub fn shutdown_expect_success(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + self.timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.diagnose(
                    "child did not close stdout / exit within the deadline after stdin EOF",
                );
            }
            match self.stdout_rx.recv_timeout(remaining) {
                // Trailing output before shutdown; keep draining until EOF.
                Ok(_) => continue,
                // stdout closed: the process is exiting, so the reap below is
                // effectively immediate.
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => self.diagnose(
                    "child did not close stdout / exit within the deadline after stdin EOF",
                ),
            }
        }
        let surface = self.surface;
        let status = self
            .child
            .wait()
            .unwrap_or_else(|error| panic!("wait on `{surface}`: {error}"));
        assert!(
            status.success(),
            "`{surface}` exited unsuccessfully: {status}\nstderr:\n{}",
            self.collect_stderr()
        );
    }

    /// Kill the child and panic with `context`, the last request sent, and the
    /// full captured stderr. Never returns.
    fn diagnose(&mut self, context: &str) -> ! {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let surface = self.surface;
        let last_request = self
            .last_request
            .clone()
            .unwrap_or_else(|| "<none>".to_string());
        let stderr = self.collect_stderr();
        panic!("{context}\nlast request: {last_request}\n`{surface}` stderr:\n{stderr}");
    }

    /// Join the stderr reader (the pipe has closed once the child exited or was
    /// killed) to recover the complete stderr transcript, falling back to
    /// draining the channel if the handle was already taken.
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

impl Drop for StdioJsonRpcClient {
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
