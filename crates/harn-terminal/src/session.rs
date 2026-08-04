use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

use crate::input::{encode_events, InputEvent};
use crate::types::{
    validate_dimensions, CellCapture, CellRegion, ProcessStatus, ScreenCapture, SessionOptions,
    TerminalError, WaitIdleResult,
};

#[derive(Default)]
struct ParserTelemetry {
    errors: usize,
}

impl ParserTelemetry {
    fn record_error(&mut self) {
        self.errors = self.errors.saturating_add(1);
    }
}

impl vt100::Callbacks for ParserTelemetry {
    fn unhandled_char(&mut self, _: &mut vt100::Screen, _: char) {
        self.record_error();
    }

    fn unhandled_control(&mut self, _: &mut vt100::Screen, _: u8) {
        self.record_error();
    }
}

struct TerminalState {
    parser: vt100::Parser<ParserTelemetry>,
    raw: VecDeque<u8>,
    raw_capacity: usize,
    raw_truncated: bool,
    bytes_received: u64,
    revision: u64,
    last_output_at: Instant,
    status: ProcessStatus,
    reader_error: Option<String>,
    reader_done: bool,
}

struct SharedState {
    state: Mutex<TerminalState>,
    changed: Condvar,
}

/// A live pseudo-terminal session with typed VT state.
pub struct TerminalSession {
    shared: Arc<SharedState>,
    writer: Mutex<Option<Box<dyn Write + Send>>>,
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    reader: Mutex<Option<JoinHandle<()>>>,
    waiter: Mutex<Option<JoinHandle<()>>>,
}

impl TerminalSession {
    /// Start a child process in a fresh native PTY.
    pub fn spawn(options: SessionOptions) -> Result<Self, TerminalError> {
        options.validate()?;
        let size = pty_size(options.rows, options.cols);
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|error| TerminalError::OpenPty(error.to_string()))?;

        let mut command = CommandBuilder::new(&options.argv[0]);
        command.args(&options.argv[1..]);
        if let Some(cwd) = options.cwd.as_ref() {
            command.cwd(cwd);
        }
        for key in &options.env_remove {
            command.env_remove(key);
        }
        for (key, value) in &options.env {
            command.env(key, value);
        }

        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| TerminalError::Spawn(error.to_string()))?;
        let killer = child.clone_killer();
        drop(pair.slave);

        let writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TerminalError::Io(error.to_string()));
            }
        };
        let mut reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TerminalError::Io(error.to_string()));
            }
        };

        let now = Instant::now();
        let shared = Arc::new(SharedState {
            state: Mutex::new(TerminalState {
                parser: vt100::Parser::new_with_callbacks(
                    options.rows,
                    options.cols,
                    0,
                    ParserTelemetry::default(),
                ),
                raw: VecDeque::with_capacity(options.raw_capacity.min(16 * 1024)),
                raw_capacity: options.raw_capacity,
                raw_truncated: false,
                bytes_received: 0,
                revision: 0,
                last_output_at: now,
                status: ProcessStatus::Running,
                reader_error: None,
                reader_done: false,
            }),
            changed: Condvar::new(),
        });

        let reader_shared = Arc::clone(&shared);
        let reader_handle = match thread::Builder::new()
            .name("harn-terminal-reader".to_string())
            .spawn(move || {
                let mut buffer = [0_u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            let Ok(mut state) = reader_shared.state.lock() else {
                                break;
                            };
                            append_raw(&mut state, &buffer[..count]);
                            state.parser.process(&buffer[..count]);
                            state.bytes_received = state
                                .bytes_received
                                .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
                            state.revision = state.revision.saturating_add(1);
                            state.last_output_at = Instant::now();
                            drop(state);
                            reader_shared.changed.notify_all();
                        }
                        Err(error) => {
                            if let Ok(mut state) = reader_shared.state.lock() {
                                if !is_normal_pty_eof(&error) {
                                    state.reader_error = Some(error.to_string());
                                }
                                state.revision = state.revision.saturating_add(1);
                            }
                            break;
                        }
                    }
                }
                if let Ok(mut state) = reader_shared.state.lock() {
                    state.reader_done = true;
                }
                reader_shared.changed.notify_all();
            }) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(TerminalError::Io(error.to_string()));
            }
        };

        let waiter_shared = Arc::clone(&shared);
        let mut killer = killer;
        let waiter_handle = match thread::Builder::new()
            .name("harn-terminal-waiter".to_string())
            .spawn(move || {
                let status = match child.wait() {
                    Ok(status) => {
                        let signal = status.signal().map(ToOwned::to_owned);
                        ProcessStatus::Exited {
                            code: signal.is_none().then(|| status.exit_code()),
                            signal,
                        }
                    }
                    Err(error) => ProcessStatus::Failed {
                        message: error.to_string(),
                    },
                };
                if let Ok(mut state) = waiter_shared.state.lock() {
                    state.status = status;
                    state.revision = state.revision.saturating_add(1);
                }
                waiter_shared.changed.notify_all();
            }) {
            Ok(handle) => handle,
            Err(error) => {
                let _ = killer.kill();
                drop(writer);
                drop(pair.master);
                let _ = reader_handle.join();
                return Err(TerminalError::Io(error.to_string()));
            }
        };

        Ok(Self {
            shared,
            writer: Mutex::new(Some(writer)),
            master: Mutex::new(Some(pair.master)),
            killer: Mutex::new(killer),
            reader: Mutex::new(Some(reader_handle)),
            waiter: Mutex::new(Some(waiter_handle)),
        })
    }

    /// Send a prevalidated batch of text and typed key events.
    pub fn send(&self, events: &[InputEvent]) -> Result<usize, TerminalError> {
        let bytes = encode_events(events)?;
        let mut writer = lock(&self.writer)?;
        let writer = writer.as_mut().ok_or(TerminalError::Closed)?;
        writer
            .write_all(&bytes)
            .and_then(|()| writer.flush())
            .map_err(|error| TerminalError::Io(error.to_string()))?;
        Ok(bytes.len())
    }

    /// Capture the current screen, optionally including detailed cells.
    pub fn capture(&self, region: Option<CellRegion>) -> Result<ScreenCapture, TerminalError> {
        let state = lock(&self.shared.state)?;
        let screen = state.parser.screen();
        let (rows, columns) = screen.size();
        if let Some(region) = region {
            region.validate(rows, columns)?;
        }
        let text_rows = screen.rows(0, columns).collect();
        let (cursor_row, cursor_column) = screen.cursor_position();
        let cells = region.map(|region| capture_cells(screen, region));
        Ok(ScreenCapture {
            schema_version: 1,
            rows,
            columns,
            text_rows,
            cursor_row,
            cursor_column,
            cursor_visible: !screen.hide_cursor(),
            alternate_screen: screen.alternate_screen(),
            revision: state.revision,
            bytes_received: state.bytes_received,
            raw_truncated: state.raw_truncated,
            parser_errors: state.parser.callbacks().errors,
            reader_error: state.reader_error.clone(),
            status: state.status.clone(),
            cells,
        })
    }

    /// Resize both the native PTY and the VT parser.
    pub fn resize(&self, rows: u16, columns: u16) -> Result<(), TerminalError> {
        validate_dimensions(rows, columns)?;
        let master = lock(&self.master)?;
        master
            .as_ref()
            .ok_or(TerminalError::Closed)?
            .resize(pty_size(rows, columns))
            .map_err(|error| TerminalError::Io(error.to_string()))?;
        drop(master);
        let mut state = lock(&self.shared.state)?;
        state.parser.screen_mut().set_size(rows, columns);
        state.revision = state.revision.saturating_add(1);
        drop(state);
        self.shared.changed.notify_all();
        Ok(())
    }

    /// Wait until no child output has arrived for `quiet`.
    pub fn wait_idle(
        &self,
        quiet: Duration,
        timeout: Duration,
    ) -> Result<WaitIdleResult, TerminalError> {
        self.wait_idle_inner(None, quiet, timeout)
    }

    /// Wait for a state revision newer than `after_revision`, then for quiet.
    ///
    /// This fences a send-then-capture sequence against output that was already
    /// quiet before the input was written. The wait remains notification-driven.
    pub fn wait_idle_after(
        &self,
        after_revision: u64,
        quiet: Duration,
        timeout: Duration,
    ) -> Result<WaitIdleResult, TerminalError> {
        self.wait_idle_inner(Some(after_revision), quiet, timeout)
    }

    fn wait_idle_inner(
        &self,
        after_revision: Option<u64>,
        quiet: Duration,
        timeout: Duration,
    ) -> Result<WaitIdleResult, TerminalError> {
        if quiet > timeout {
            return Err(TerminalError::InvalidArgument(
                "quiet duration must not exceed timeout".to_string(),
            ));
        }
        let started = Instant::now();
        let deadline = started + timeout;
        let mut state = lock(&self.shared.state)?;
        loop {
            let now = Instant::now();
            let quiet_for = now.saturating_duration_since(state.last_output_at);
            let fully_exited = state.status.finished() && state.reader_done;
            let observed_output = state.bytes_received > 0;
            let crossed_fence = after_revision.is_none_or(|revision| state.revision > revision);
            if fully_exited
                || !state.status.finished()
                    && observed_output
                    && crossed_fence
                    && quiet_for >= quiet
            {
                return Ok(WaitIdleResult {
                    revision: state.revision,
                    quiet_ms: duration_ms(quiet),
                    status: state.status.clone(),
                });
            }
            if now >= deadline {
                return Err(TerminalError::Timeout {
                    operation: "idle output",
                    timeout_ms: duration_ms(timeout),
                });
            }
            let remaining_quiet = quiet.saturating_sub(quiet_for);
            let remaining_timeout = deadline.saturating_duration_since(now);
            let wait_for = if state.status.finished() || !observed_output || !crossed_fence {
                remaining_timeout
            } else {
                remaining_quiet.min(remaining_timeout)
            };
            let (next, _) = self
                .shared
                .changed
                .wait_timeout(state, wait_for)
                .map_err(|_| TerminalError::Poisoned)?;
            state = next;
        }
    }

    /// Wait for the child process to reach a terminal state.
    pub fn wait_exit(&self, timeout: Duration) -> Result<ProcessStatus, TerminalError> {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.shared.state)?;
        loop {
            if state.status.finished() && state.reader_done {
                return Ok(state.status.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(TerminalError::Timeout {
                    operation: "process exit",
                    timeout_ms: duration_ms(timeout),
                });
            }
            let (next, _) = self
                .shared
                .changed
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .map_err(|_| TerminalError::Poisoned)?;
            state = next;
        }
    }

    /// Terminate the child, close terminal handles, and join worker threads.
    pub fn end(&self, timeout: Duration) -> Result<ProcessStatus, TerminalError> {
        let running = !lock(&self.shared.state)?.status.finished();
        let kill_error = if running {
            lock(&self.killer)?.kill().err()
        } else {
            None
        };
        lock(&self.writer)?.take();
        lock(&self.master)?.take();
        let status = match self.wait_exit(timeout) {
            Ok(status) => status,
            Err(wait_error) => {
                return Err(match kill_error {
                    Some(kill_error) => TerminalError::Io(format!(
                        "failed to terminate child ({kill_error}); {wait_error}"
                    )),
                    None => wait_error,
                });
            }
        };
        join_handle(&self.waiter)?;
        join_handle(&self.reader)?;
        Ok(status)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.end(Duration::from_secs(2));
    }
}

fn pty_size(rows: u16, cols: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn is_normal_pty_eof(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::BrokenPipe
        || error.kind() == std::io::ErrorKind::UnexpectedEof
        || cfg!(unix) && error.raw_os_error() == Some(5)
}

fn append_raw(state: &mut TerminalState, bytes: &[u8]) {
    let overflow = state
        .raw
        .len()
        .saturating_add(bytes.len())
        .saturating_sub(state.raw_capacity);
    if overflow > 0 {
        let remove = overflow.min(state.raw.len());
        state.raw.drain(..remove);
        state.raw_truncated = true;
    }
    let start = bytes.len().saturating_sub(state.raw_capacity);
    if start > 0 {
        state.raw_truncated = true;
    }
    state.raw.extend(&bytes[start..]);
}

fn capture_cells(screen: &vt100::Screen, region: CellRegion) -> Vec<CellCapture> {
    let mut cells = Vec::with_capacity(usize::from(region.rows) * usize::from(region.columns));
    for row in region.row..region.row + region.rows {
        for column in region.column..region.column + region.columns {
            if let Some(cell) = screen.cell(row, column) {
                cells.push(CellCapture {
                    row,
                    column,
                    text: cell.contents().to_owned(),
                    foreground: cell.fgcolor().into(),
                    background: cell.bgcolor().into(),
                    bold: cell.bold(),
                    italic: cell.italic(),
                    underline: cell.underline(),
                    inverse: cell.inverse(),
                    wide: cell.is_wide(),
                    wide_continuation: cell.is_wide_continuation(),
                });
            }
        }
    }
    cells
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, TerminalError> {
    mutex.lock().map_err(|_| TerminalError::Poisoned)
}

fn join_handle(handle: &Mutex<Option<JoinHandle<()>>>) -> Result<(), TerminalError> {
    if let Some(handle) = lock(handle)?.take() {
        handle.join().map_err(|_| TerminalError::Poisoned)?;
    }
    Ok(())
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
