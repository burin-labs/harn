//! A synchronous LSP client over a child's stdio.
//!
//! One reader thread demultiplexes responses to waiting callers, answers the
//! few server-to-client requests servers block on, and tracks readiness. The
//! child is spawned through the hostlib process spawner, so the process
//! sandbox and environment scrubbing apply to language servers as well.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::host_call::{HostCallContext, Interrupt, Phase, INTERRUPT_POLL};
use crate::process::{
    spawn_process, EnvMode, OutputCapture, OwnerDeathPolicy, ProcessKiller, SpawnSpec,
};

use super::servers::{Launch, Readiness, ServerSpec};

const STDERR_TAIL_BYTES: usize = 2048;
/// JSON-RPC codes a server uses when a request raced a document or project
/// change; the request is retryable rather than refused.
const RETRYABLE_CODES: [i64; 3] = [-32801, -32802, -32800];

#[derive(Debug)]
pub(crate) enum RpcError {
    Timeout,
    /// The server process ended; carries the tail of its stderr.
    Exited(String),
    /// The server answered with an error object.
    Server {
        code: i64,
        message: String,
    },
    Protocol(String),
    /// The caller stopped the call before the server answered.
    Interrupted(Interrupt),
}

impl RpcError {
    pub(crate) fn retryable(&self) -> bool {
        matches!(self, Self::Server { code, .. } if RETRYABLE_CODES.contains(code))
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => f.write_str("request timed out"),
            Self::Exited(tail) if tail.is_empty() => f.write_str("server exited"),
            Self::Exited(tail) => write!(f, "server exited: {tail}"),
            Self::Server { code, message } => write!(f, "server error {code}: {message}"),
            Self::Protocol(detail) => write!(f, "protocol error: {detail}"),
            Self::Interrupted(Interrupt::Cancelled) => f.write_str("cancelled"),
            Self::Interrupted(Interrupt::DeadlineExceeded) => f.write_str("deadline exceeded"),
        }
    }
}

type Reply = Result<Value, RpcError>;

#[derive(Default)]
struct Status {
    exited: bool,
    /// Work-done progress tokens that have begun and not ended.
    active_progress: std::collections::HashSet<String>,
    /// How many progress tokens have ended since the server started.
    progress_ends: u64,
    /// rust-analyzer's `experimental/serverStatus` quiescence.
    quiescent: bool,
    stderr_tail: Vec<u8>,
}

struct Shared {
    pending: Mutex<HashMap<i64, Sender<Reply>>>,
    status: Mutex<Status>,
    changed: Condvar,
}

impl Shared {
    fn stderr_tail(&self) -> String {
        let status = self.status.lock().expect("lsp status poisoned");
        String::from_utf8_lossy(&status.stderr_tail)
            .trim()
            .to_string()
    }
}

/// Frames bound for the server's stdin. One thread owns the pipe, so a server
/// that stops reading blocks that thread, never a caller watching for a stop.
type Writer = Sender<Vec<u8>>;

pub(crate) struct Connection {
    writer: Writer,
    shared: Arc<Shared>,
    next_id: AtomicI64,
    killer: Arc<dyn ProcessKiller>,
    #[cfg(all(test, target_os = "linux"))]
    pid: Option<u32>,
    /// The thread that waits on the child, so a test can join it.
    #[cfg(all(test, target_os = "linux"))]
    reader: Option<std::thread::JoinHandle<()>>,
}

/// What a server needs beyond the inherited environment. Discovery can find a
/// server outside `PATH`, so its own directory goes first: `cargo` beside a
/// rustup proxy, `node` for an npm launcher's `#!/usr/bin/env node`. The rustup
/// proxy finds its toolchain through `RUSTUP_HOME`, resolved as the sandbox
/// resolves it for every other child.
fn server_env(program: &str) -> std::collections::BTreeMap<String, String> {
    let mut env = std::collections::BTreeMap::new();
    let program = Path::new(program);
    if let Some(dir) = program.parent().filter(|_| program.is_absolute()) {
        let inherited = child_path(program);
        let rest = std::env::split_paths(&inherited).filter(|entry| entry != dir);
        if let Ok(path) = std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(rest)) {
            env.insert("PATH".to_string(), path.to_string_lossy().into_owned());
        }
    }
    for (name, path) in harn_vm::process_sandbox::rust_toolchain_homes() {
        env.insert(name.to_string(), path.display().to_string());
    }
    env
}

/// The `PATH` the active session gives a child running `program`, so the
/// override only prepends and never reopens a `PATH` the session closed.
fn child_path(program: &Path) -> std::ffi::OsString {
    let Ok(command) = harn_vm::process_sandbox::session_std_command(program) else {
        return std::ffi::OsString::new();
    };
    match command.get_envs().find(|(key, _)| *key == "PATH") {
        Some((_, value)) => value.map(ToOwned::to_owned).unwrap_or_default(),
        None if harn_vm::stdlib::process::current_session_environment().is_some() => {
            std::ffi::OsString::new()
        }
        None => std::env::var_os("PATH").unwrap_or_default(),
    }
}

impl Connection {
    pub(crate) fn spawn(
        spec: &ServerSpec,
        launch: &Launch,
        program: &str,
        root: &Path,
    ) -> Result<Self, String> {
        let mut handle = spawn_process(SpawnSpec {
            builtin: "hostlib_code_index_rename_symbol",
            program: program.to_string(),
            args: launch.args.iter().map(|arg| arg.to_string()).collect(),
            cwd: Some(root.to_path_buf()),
            env: server_env(program),
            env_remove: Vec::new(),
            env_mode: EnvMode::InheritClean,
            use_stdin: true,
            configure_process_group: true,
            owner_death: OwnerDeathPolicy::None,
            output_capture: OutputCapture::Pipe,
        })
        .map_err(|err| format!("could not start `{program}`: {err}"))?;
        let killer = handle.killer();
        #[cfg(all(test, target_os = "linux"))]
        let pid = handle.pid();
        let (Some(stdin), Some(stdout), Some(stderr)) = (
            handle.take_stdin(),
            handle.take_stdout(),
            handle.take_stderr(),
        ) else {
            killer.kill();
            return Err(format!("`{program}` started without stdio pipes"));
        };
        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            status: Mutex::new(Status::default()),
            changed: Condvar::new(),
        });
        let (writer, frames) = channel::<Vec<u8>>();
        harn_parser::runtime_stack::builder()
            .name(format!("lsp-writer-{}", launch.id))
            .spawn(move || write_loop(stdin, frames))
            .map_err(|err| format!("spawn writer: {err}"))?;
        #[cfg_attr(not(all(test, target_os = "linux")), allow(unused_mut))]
        let mut connection = Self {
            writer: writer.clone(),
            shared: shared.clone(),
            next_id: AtomicI64::new(1),
            killer,
            #[cfg(all(test, target_os = "linux"))]
            pid,
            #[cfg(all(test, target_os = "linux"))]
            reader: None,
        };
        let stderr_shared = shared.clone();
        harn_parser::runtime_stack::builder()
            .name(format!("lsp-stderr-{}", launch.id))
            .spawn(move || drain_stderr(stderr, &stderr_shared))
            .map_err(|err| format!("spawn stderr reader: {err}"))?;
        let settings = spec.settings();
        let _reader = harn_parser::runtime_stack::builder()
            .name(format!("lsp-reader-{}", launch.id))
            .spawn(move || {
                read_loop(stdout, &shared, &writer, settings);
                // The server closed its output, so it has exited or is being
                // killed; wait for it so no zombie outlives the session.
                let _ = handle.wait();
            })
            .map_err(|err| format!("spawn reader: {err}"))?;
        #[cfg(all(test, target_os = "linux"))]
        {
            connection.reader = Some(_reader);
        }
        Ok(connection)
    }

    pub(crate) fn request<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
        timeout: Duration,
        call: &HostCallContext,
    ) -> Result<R, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = channel();
        self.shared
            .pending
            .lock()
            .expect("lsp pending poisoned")
            .insert(id, tx);
        if self.exited() {
            self.forget(id);
            return Err(RpcError::Exited(self.shared.stderr_tail()));
        }
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + timeout;
        let value = loop {
            if let Some(interrupt) = call.interrupted() {
                self.abandon(id);
                return Err(RpcError::Interrupted(interrupt));
            }
            let now = Instant::now();
            if now >= deadline {
                self.abandon(id);
                return Err(RpcError::Timeout);
            }
            match rx.recv_timeout(INTERRUPT_POLL.min(deadline - now)) {
                Ok(reply) => break reply?,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(RpcError::Exited(self.shared.stderr_tail()))
                }
            }
        };
        serde_json::from_value(value).map_err(|err| RpcError::Protocol(format!("{method}: {err}")))
    }

    pub(crate) fn notify<P: Serialize>(&self, method: &str, params: P) -> Result<(), RpcError> {
        self.write(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// Stop waiting for request `id` and tell the server to drop it.
    fn abandon(&self, id: i64) {
        self.forget(id);
        let _ = self.notify("$/cancelRequest", json!({"id": id}));
    }

    fn forget(&self, id: i64) {
        self.shared
            .pending
            .lock()
            .expect("lsp pending poisoned")
            .remove(&id);
    }

    fn write(&self, message: &Value) -> Result<(), RpcError> {
        write_frame(&self.writer, message).map_err(|_| RpcError::Exited(self.shared.stderr_tail()))
    }

    pub(crate) fn progress_ends(&self) -> u64 {
        self.shared
            .status
            .lock()
            .expect("lsp status poisoned")
            .progress_ends
    }

    /// Wait until no work-done progress is active. With `ends_before`, a
    /// progress cycle is expected after that count: wait up to `grace` for
    /// one to begin, then for it to end. `Ok(false)` means progress was
    /// still active at `deadline`.
    pub(crate) fn wait_progress(
        &self,
        ends_before: Option<u64>,
        grace: Duration,
        deadline: Instant,
        call: &HostCallContext,
        server: &str,
    ) -> Result<bool, RpcError> {
        let grace_until = Instant::now() + grace;
        let mut status = self.shared.status.lock().expect("lsp status poisoned");
        loop {
            if status.exited {
                drop(status);
                return Err(RpcError::Exited(self.shared.stderr_tail()));
            }
            let now = Instant::now();
            if status.active_progress.is_empty() {
                let ended = ends_before.is_none_or(|before| status.progress_ends > before);
                if ended || now >= grace_until {
                    return Ok(true);
                }
            }
            if now >= deadline {
                return Ok(false);
            }
            if let Some(interrupt) = call.interrupted() {
                return Err(RpcError::Interrupted(interrupt));
            }
            call.heartbeat(Phase::Indexing, server);
            let until = if status.active_progress.is_empty() {
                grace_until.min(deadline)
            } else {
                deadline
            };
            status = self
                .shared
                .changed
                .wait_timeout(
                    status,
                    INTERRUPT_POLL.min(until.saturating_duration_since(now)),
                )
                .expect("lsp status poisoned")
                .0;
        }
    }

    pub(crate) fn exited(&self) -> bool {
        self.shared
            .status
            .lock()
            .expect("lsp status poisoned")
            .exited
    }

    /// Wait until the server reports it can answer semantic queries, or
    /// `deadline` passes. `Ok(false)` is a timeout; the server keeps warming.
    pub(crate) fn wait_ready(
        &self,
        readiness: Readiness,
        deadline: Instant,
        call: &HostCallContext,
        server: &str,
    ) -> Result<bool, RpcError> {
        if readiness != Readiness::Quiescent {
            return Ok(true);
        }
        let mut status = self.shared.status.lock().expect("lsp status poisoned");
        loop {
            if status.exited {
                drop(status);
                return Err(RpcError::Exited(self.shared.stderr_tail()));
            }
            if status.quiescent {
                return Ok(true);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            if let Some(interrupt) = call.interrupted() {
                return Err(RpcError::Interrupted(interrupt));
            }
            call.heartbeat(Phase::ServerStart, server);
            status = self
                .shared
                .changed
                .wait_timeout(status, INTERRUPT_POLL.min(deadline - now))
                .expect("lsp status poisoned")
                .0;
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Servers exit on stdin EOF; the kill covers one that does not.
        let _ = self.notify("exit", Value::Null);
        self.killer.kill();
    }
}

fn write_frame(writer: &Writer, message: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(message)?;
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    writer
        .send(frame)
        .map_err(|_| std::io::ErrorKind::BrokenPipe.into())
}

/// Write queued frames in order until the pipe breaks or every sender drops.
fn write_loop(mut stdin: Box<dyn Write + Send>, frames: std::sync::mpsc::Receiver<Vec<u8>>) {
    for frame in frames {
        if stdin
            .write_all(&frame)
            .and_then(|()| stdin.flush())
            .is_err()
        {
            break;
        }
    }
}

fn drain_stderr(mut stderr: Box<dyn Read + Send>, shared: &Shared) {
    let mut chunk = [0u8; 1024];
    while let Ok(read) = stderr.read(&mut chunk) {
        if read == 0 {
            break;
        }
        let mut status = shared.status.lock().expect("lsp status poisoned");
        status.stderr_tail.extend_from_slice(&chunk[..read]);
        let overflow = status.stderr_tail.len().saturating_sub(STDERR_TAIL_BYTES);
        status.stderr_tail.drain(..overflow);
    }
}

fn read_message(reader: &mut impl BufRead) -> Option<Value> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length:") {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let mut body = vec![0; length?];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn read_loop(
    stdout: Box<dyn Read + Send>,
    shared: &Shared,
    writer: &Writer,
    settings: fn(&str) -> Value,
) {
    let mut reader = BufReader::new(stdout);
    while let Some(message) = read_message(&mut reader) {
        let method = message.get("method").and_then(Value::as_str);
        match (message.get("id"), method) {
            (Some(id), None) => {
                let Some(id) = id.as_i64() else { continue };
                let sender = shared
                    .pending
                    .lock()
                    .expect("lsp pending poisoned")
                    .remove(&id);
                if let Some(sender) = sender {
                    let reply = match message.get("error") {
                        Some(error) => Err(RpcError::Server {
                            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                            message: error
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        }),
                        None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = sender.send(reply);
                }
            }
            (Some(id), Some(method)) => {
                let _ = write_frame(
                    writer,
                    &answer_server_request(id.clone(), method, &message, settings),
                );
            }
            (None, Some("$/progress")) => {
                let token = message
                    .pointer("/params/token")
                    .map(|token| token.to_string())
                    .unwrap_or_default();
                let kind = message
                    .pointer("/params/value/kind")
                    .and_then(Value::as_str);
                let mut status = shared.status.lock().expect("lsp status poisoned");
                match kind {
                    Some("begin") => {
                        status.active_progress.insert(token);
                    }
                    Some("end") => {
                        status.active_progress.remove(&token);
                        status.progress_ends += 1;
                    }
                    _ => {}
                }
                shared.changed.notify_all();
            }
            (None, Some("experimental/serverStatus")) => {
                let quiescent = message
                    .pointer("/params/quiescent")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let mut status = shared.status.lock().expect("lsp status poisoned");
                status.quiescent = quiescent;
                shared.changed.notify_all();
            }
            _ => {}
        }
    }
    let mut status = shared.status.lock().expect("lsp status poisoned");
    status.exited = true;
    shared.changed.notify_all();
    drop(status);
    // Wake every waiter: dropping the senders disconnects their receivers.
    shared.pending.lock().expect("lsp pending poisoned").clear();
}

fn answer_server_request(
    id: Value,
    method: &str,
    message: &Value,
    settings: fn(&str) -> Value,
) -> Value {
    let result = match method {
        // Servers block startup on configuration; the spec's settings also
        // keep expensive background checks off.
        "workspace/configuration" => Value::Array(
            message
                .pointer("/params/items")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            settings(item.get("section").and_then(Value::as_str).unwrap_or(""))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        "window/workDoneProgress/create"
        | "client/registerCapability"
        | "client/unregisterCapability"
        | "window/showMessageRequest" => Value::Null,
        "workspace/applyEdit" => json!({"applied": false}),
        _ => {
            return json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}})
        }
    };
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::code_index::lsp::servers;

    /// A server that ends with its session must be reaped then, not when the
    /// host exits: an unreaped child stays a zombie with a `/proc` entry.
    #[test]
    fn a_dropped_session_reaps_its_server() {
        let spec = servers::for_language(crate::ast::Language::Rust).expect("rust spec");
        let launch = &spec.launches[0];
        let _env = crate::process::test_support::declare_inherited();
        let root = tempfile::tempdir().unwrap();
        let mut connection = Connection::spawn(spec, launch, "cat", root.path()).expect("spawn");
        let pid = connection.pid.expect("pid");
        let status = format!("/proc/{pid}/status");
        assert!(Path::new(&status).exists());
        let reader = connection.reader.take().expect("reader thread");
        drop(connection);
        // The reader returns only after it has waited on the child.
        reader.join().expect("reader thread");
        let state = std::fs::read_to_string(&status).unwrap_or_default();
        assert!(
            !Path::new(&status).exists(),
            "server {pid} was not reaped: {}",
            state.lines().find(|l| l.starts_with("State")).unwrap_or("")
        );
    }
}
