use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
#[cfg(not(unix))]
use std::io::BufRead;
use std::io::{IsTerminal, Read, Write};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
#[cfg(unix)]
use std::time::{Duration, Instant};

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::stdlib::options::{self, ErrorKind, OptionsParser};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::logging::{vm_build_log_line, vm_escape_json_str_quoted, VM_MIN_LOG_LEVEL};

#[derive(Clone, Copy, Default)]
struct TtyMock {
    stdin: Option<bool>,
    stdout: Option<bool>,
    stderr: Option<bool>,
}

#[derive(Clone, Copy, Default, PartialEq)]
enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Clone, Debug)]
struct ReadLineOptions {
    prompt: String,
    timeout_ms: Option<u64>,
    trim: bool,
    echo: bool,
    raw: bool,
}

impl Default for ReadLineOptions {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            timeout_ms: None,
            trim: true,
            echo: true,
            raw: false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ReadLineOutcome {
    Ok(String),
    Eof,
    #[cfg(unix)]
    Timeout,
    #[cfg(unix)]
    Interrupt,
    Error(String),
}

enum MockReadLine {
    Line(String),
    Eof,
    Unset,
}

thread_local! {
    static STDIN_MOCK: RefCell<Option<String>> = const { RefCell::new(None) };
    static STDIN_LINES: RefCell<Option<VecDeque<String>>> = const { RefCell::new(None) };
    static STDERR_BUFFER: RefCell<String> = const { RefCell::new(String::new()) };
    static STDERR_CAPTURING: RefCell<bool> = const { RefCell::new(false) };
    static STDOUT_PASSTHROUGH: RefCell<bool> = const { RefCell::new(false) };
    static TTY_MOCK: RefCell<TtyMock> = const { RefCell::new(TtyMock { stdin: None, stdout: None, stderr: None }) };
    static COLOR_MODE: RefCell<ColorMode> = const { RefCell::new(ColorMode::Auto) };
}

static STDIN_READ_LOCK: Mutex<()> = Mutex::new(());

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &LOG_BUILTIN_DEF,
    &COLOR_BUILTIN_DEF,
    &BOLD_BUILTIN_DEF,
    &DIM_BUILTIN_DEF,
    &SET_COLOR_MODE_BUILTIN_DEF,
    &ANSI_ENABLED_BUILTIN_DEF,
    &READ_STDIN_BUILTIN_DEF,
    &IO_READ_LINE_BUILTIN_DEF,
    &IO_WRITE_STDERR_BUILTIN_DEF,
    &IO_WRITE_STDOUT_BUILTIN_DEF,
    &IO_PRINT_BUILTIN_DEF,
    &IO_PRINTLN_BUILTIN_DEF,
    &IO_EPRINT_BUILTIN_DEF,
    &IO_EPRINTLN_BUILTIN_DEF,
    &IS_STDIN_TTY_BUILTIN_DEF,
    &IS_STDOUT_TTY_BUILTIN_DEF,
    &IS_STDERR_TTY_BUILTIN_DEF,
    &MOCK_STDIN_BUILTIN_DEF,
    &UNMOCK_STDIN_BUILTIN_DEF,
    &MOCK_TTY_BUILTIN_DEF,
    &UNMOCK_TTY_BUILTIN_DEF,
    &CAPTURE_STDERR_START_BUILTIN_DEF,
    &CAPTURE_STDERR_TAKE_BUILTIN_DEF,
    &UUID_BUILTIN_DEF,
    &UUID_PARSE_BUILTIN_DEF,
    &UUID_V7_BUILTIN_DEF,
    &UUID_V5_BUILTIN_DEF,
    &UUID_NIL_BUILTIN_DEF,
    &LOG_DEBUG_BUILTIN_DEF,
    &LOG_INFO_BUILTIN_DEF,
    &LOG_WARN_BUILTIN_DEF,
    &LOG_ERROR_BUILTIN_DEF,
    &LOG_SET_LEVEL_BUILTIN_DEF,
    &PROGRESS_BUILTIN_DEF,
    &LOG_JSON_BUILTIN_DEF,
];

/// Reset all io thread-local state for test isolation.
pub(crate) fn reset_io_state() {
    STDIN_MOCK.with(|s| *s.borrow_mut() = None);
    STDIN_LINES.with(|s| *s.borrow_mut() = None);
    STDERR_BUFFER.with(|s| s.borrow_mut().clear());
    STDERR_CAPTURING.with(|s| *s.borrow_mut() = false);
    STDOUT_PASSTHROUGH.with(|s| *s.borrow_mut() = false);
    TTY_MOCK.with(|t| *t.borrow_mut() = TtyMock::default());
    COLOR_MODE.with(|m| *m.borrow_mut() = ColorMode::Auto);
}

/// Enable or disable direct stdout writes for CLI-style runs.
///
/// The VM normally captures stdout in-memory so tests and embedding callers
/// can inspect it after execution. Interactive CLI programs need prompts to
/// appear before `read_line()` blocks, so `harn run` enables this mode and
/// streams `print`/`println`/`log` immediately.
pub fn set_stdout_passthrough(enabled: bool) -> bool {
    STDOUT_PASSTHROUGH.with(|state| {
        let previous = *state.borrow();
        *state.borrow_mut() = enabled;
        previous
    })
}

/// Drain and return the buffered stderr output. The CLI flushes this to
/// the real stderr at the end of execution.
pub fn take_stderr_buffer() -> String {
    STDERR_BUFFER.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

pub(crate) fn write_stderr(line: &str) {
    if crate::run_events::sink_active() {
        crate::run_events::emit(crate::run_events::RunEvent::Stderr {
            payload: line.to_string(),
        });
        return;
    }
    let capturing = STDERR_CAPTURING.with(|c| *c.borrow());
    if capturing {
        STDERR_BUFFER.with(|s| s.borrow_mut().push_str(line));
    } else {
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(line.as_bytes());
        let _ = stderr.flush();
    }
}

pub(crate) fn write_stdout(out: &mut String, text: &str) {
    if crate::run_events::sink_active() {
        crate::run_events::emit(crate::run_events::RunEvent::Stdout {
            payload: text.to_string(),
        });
        return;
    }
    if stdout_passthrough_enabled() {
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(text.as_bytes());
        let _ = stdout.flush();
    } else {
        out.push_str(text);
    }
}

fn stdout_passthrough_enabled() -> bool {
    STDOUT_PASSTHROUGH.with(|state| *state.borrow())
}

fn read_stdin_all_real() -> Option<String> {
    let mut buf = String::new();
    if std::io::stdin().lock().read_to_string(&mut buf).is_ok() {
        Some(buf)
    } else {
        None
    }
}

#[cfg(not(unix))]
fn read_stdin_line_real() -> Option<String> {
    let mut buf = String::new();
    if std::io::stdin().lock().read_line(&mut buf).is_ok() {
        if buf.is_empty() {
            None
        } else {
            // Trim trailing \n / \r\n but keep internal whitespace.
            if buf.ends_with('\n') {
                buf.pop();
                if buf.ends_with('\r') {
                    buf.pop();
                }
            }
            Some(buf)
        }
    } else {
        None
    }
}

fn pop_mock_line() -> MockReadLine {
    STDIN_LINES.with(|lines| {
        let mut borrow = lines.borrow_mut();
        if let Some(queue) = borrow.as_mut() {
            return queue
                .pop_front()
                .map(MockReadLine::Line)
                .unwrap_or(MockReadLine::Eof);
        }
        MockReadLine::Unset
    })
}

fn read_mock_line() -> MockReadLine {
    match pop_mock_line() {
        MockReadLine::Unset => {}
        other => return other,
    }
    let bulk = STDIN_MOCK.with(|s| s.borrow_mut().take());
    let Some(text) = bulk else {
        return MockReadLine::Unset;
    };
    let mut lines: VecDeque<String> = text.split('\n').map(String::from).collect();
    // Keep legacy read_line semantics: a final newline terminates the last
    // line rather than producing one more empty line.
    if matches!(lines.back(), Some(line) if line.is_empty()) {
        lines.pop_back();
    }
    let first = lines.pop_front();
    STDIN_LINES.with(|q| *q.borrow_mut() = Some(lines));
    first.map(MockReadLine::Line).unwrap_or(MockReadLine::Eof)
}

fn normalize_read_line_value(mut line: String, trim: bool) -> String {
    if line.ends_with('\r') {
        line.pop();
    }
    if trim {
        line.trim().to_string()
    } else {
        line
    }
}

fn read_line_result(outcome: ReadLineOutcome) -> VmValue {
    let mut out = BTreeMap::new();
    match outcome {
        ReadLineOutcome::Ok(value) => {
            out.insert("ok".to_string(), VmValue::Bool(true));
            out.insert("status".to_string(), VmValue::string("ok"));
            out.insert("value".to_string(), VmValue::string(value));
        }
        ReadLineOutcome::Eof => {
            out.insert("ok".to_string(), VmValue::Bool(false));
            out.insert("status".to_string(), VmValue::string("eof"));
        }
        #[cfg(unix)]
        ReadLineOutcome::Timeout => {
            out.insert("ok".to_string(), VmValue::Bool(false));
            out.insert("status".to_string(), VmValue::string("timeout"));
        }
        #[cfg(unix)]
        ReadLineOutcome::Interrupt => {
            out.insert("ok".to_string(), VmValue::Bool(false));
            out.insert("status".to_string(), VmValue::string("interrupt"));
        }
        ReadLineOutcome::Error(error) => {
            out.insert("ok".to_string(), VmValue::Bool(false));
            out.insert("status".to_string(), VmValue::string("error"));
            out.insert("error".to_string(), VmValue::string(error));
        }
    }
    VmValue::dict(out)
}

const READ_LINE_FN: &str = "std/io.read_line";

fn parse_read_line_timeout_ms(value: Option<&VmValue>) -> Result<Option<u64>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Int(value)) | Some(VmValue::Duration(value)) => {
            if *value < 0 {
                return Err(VmError::Runtime(format!(
                    "{READ_LINE_FN}: `timeout_ms` must be non-negative"
                )));
            }
            Ok(Some(*value as u64))
        }
        Some(value) => Err(VmError::Runtime(format!(
            "{READ_LINE_FN}: `timeout_ms` must be an int, duration, or nil (got {})",
            value.type_name()
        ))),
    }
}

fn parse_read_line_options(args: &[VmValue]) -> Result<ReadLineOptions, VmError> {
    if args.len() > 1 {
        return Err(VmError::Runtime(format!(
            "{READ_LINE_FN}: expected at most one options dict"
        )));
    }
    let Some(dict) =
        options::optional_dict_arg(args, 0, READ_LINE_FN, "options", ErrorKind::Runtime)?
    else {
        return Ok(ReadLineOptions::default());
    };
    let mut parser = OptionsParser::new(READ_LINE_FN, dict, ErrorKind::Runtime);
    let options = ReadLineOptions {
        prompt: parser.optional_string_raw("prompt")?.unwrap_or_default(),
        timeout_ms: parse_read_line_timeout_ms(parser.raw("timeout_ms"))?,
        trim: parser.bool_or("trim", true)?,
        echo: parser.bool_or("echo", true)?,
        raw: parser.bool_or("raw", false)?,
    };
    parser.finish_strict(&[])?;
    Ok(options)
}

fn read_line_from_mock_or_real(options: &ReadLineOptions) -> ReadLineOutcome {
    let _lock = match STDIN_READ_LOCK.lock() {
        Ok(lock) => lock,
        Err(_) => return ReadLineOutcome::Error("stdin read lock is poisoned".to_string()),
    };
    if !options.prompt.is_empty() {
        write_stderr(&options.prompt);
    }
    match read_mock_line() {
        MockReadLine::Line(line) => {
            return ReadLineOutcome::Ok(normalize_read_line_value(line, options.trim));
        }
        MockReadLine::Eof => return ReadLineOutcome::Eof,
        MockReadLine::Unset => {}
    }
    read_stdin_line_real_with_options(options)
}

#[cfg(unix)]
struct TerminalModeGuard {
    fd: libc::c_int,
    original: Option<libc::termios>,
}

#[cfg(unix)]
impl TerminalModeGuard {
    fn install(fd: libc::c_int, options: &ReadLineOptions) -> Result<Self, String> {
        let mut original = std::mem::MaybeUninit::<libc::termios>::uninit();
        let fd_is_terminal = unsafe { libc::isatty(fd) == 1 };
        if !fd_is_terminal || (options.echo && !options.raw) {
            return Ok(Self { fd, original: None });
        }
        if unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let original = unsafe { original.assume_init() };
        let mut updated = original;
        if !options.echo {
            updated.c_lflag &= !libc::ECHO;
        }
        if options.raw {
            updated.c_lflag &= !libc::ICANON;
            updated.c_cc[libc::VMIN] = 0;
            updated.c_cc[libc::VTIME] = 0;
        }
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw const updated) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Self {
            fd,
            original: Some(original),
        })
    }
}

#[cfg(unix)]
impl Drop for TerminalModeGuard {
    fn drop(&mut self) {
        if let Some(original) = &self.original {
            let _ = unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, original) };
        }
    }
}

#[cfg(unix)]
const READ_LINE_INTERRUPT_POLL: Duration = Duration::from_millis(20);

#[cfg(unix)]
fn read_line_elapsed_ms(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(unix)]
fn read_line_timeout_remaining_ms(options: &ReadLineOptions, start: Instant) -> Option<u64> {
    let timeout_ms = options.timeout_ms?;
    Some(timeout_ms.saturating_sub(read_line_elapsed_ms(start)))
}

#[cfg(unix)]
fn read_line_timed_out(options: &ReadLineOptions, start: Instant) -> bool {
    matches!(read_line_timeout_remaining_ms(options, start), Some(0))
}

#[cfg(unix)]
fn read_line_interrupt_poll_ms() -> libc::c_int {
    READ_LINE_INTERRUPT_POLL
        .as_millis()
        .min(libc::c_int::MAX as u128) as libc::c_int
}

#[cfg(unix)]
fn poll_timeout(options: &ReadLineOptions, start: Instant) -> libc::c_int {
    let heartbeat = crate::op_interrupt::installed().then_some(read_line_interrupt_poll_ms());
    match (read_line_timeout_remaining_ms(options, start), heartbeat) {
        (Some(remaining), Some(heartbeat)) => {
            remaining.min(heartbeat as u64).min(libc::c_int::MAX as u64) as libc::c_int
        }
        (Some(remaining), None) => remaining.min(libc::c_int::MAX as u64) as libc::c_int,
        (None, Some(heartbeat)) => heartbeat,
        (None, None) => -1,
    }
}

#[cfg(unix)]
fn finish_read_line(bytes: Vec<u8>, trim: bool) -> ReadLineOutcome {
    match String::from_utf8(bytes) {
        Ok(line) => ReadLineOutcome::Ok(normalize_read_line_value(line, trim)),
        Err(_) => ReadLineOutcome::Error("stdin line was not valid UTF-8".to_string()),
    }
}

#[cfg(unix)]
fn read_line_from_fd_unix(fd: libc::c_int, options: &ReadLineOptions) -> ReadLineOutcome {
    let _terminal_mode = match TerminalModeGuard::install(fd, options) {
        Ok(guard) => guard,
        Err(error) => return ReadLineOutcome::Error(error),
    };
    let start = Instant::now();
    let mut bytes = Vec::new();
    loop {
        if crate::op_interrupt::requested() {
            return ReadLineOutcome::Interrupt;
        }
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&raw mut pollfd, 1, poll_timeout(options, start)) };
        if ready == 0 {
            if read_line_timed_out(options, start) {
                return ReadLineOutcome::Timeout;
            }
            continue;
        }
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                return ReadLineOutcome::Interrupt;
            }
            return ReadLineOutcome::Error(error.to_string());
        }
        if pollfd.revents & libc::POLLNVAL != 0 {
            return ReadLineOutcome::Error("stdin fd is invalid".to_string());
        }
        if pollfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
            continue;
        }
        let mut byte = [0u8; 1];
        let read = unsafe { libc::read(fd, byte.as_mut_ptr().cast(), 1) };
        if read == 0 {
            return if bytes.is_empty() {
                ReadLineOutcome::Eof
            } else {
                finish_read_line(bytes, options.trim)
            };
        }
        if read < 0 {
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR) => return ReadLineOutcome::Interrupt,
                Some(libc::EAGAIN) => continue,
                _ => return ReadLineOutcome::Error(error.to_string()),
            }
        }
        match byte[0] {
            b'\n' => return finish_read_line(bytes, options.trim),
            b'\r' if options.raw => return finish_read_line(bytes, options.trim),
            0x03 if options.raw => return ReadLineOutcome::Interrupt,
            0x04 if options.raw && bytes.is_empty() => return ReadLineOutcome::Eof,
            0x04 if options.raw => return finish_read_line(bytes, options.trim),
            value => bytes.push(value),
        }
    }
}

#[cfg(unix)]
fn read_stdin_line_real_with_options(options: &ReadLineOptions) -> ReadLineOutcome {
    read_line_from_fd_unix(libc::STDIN_FILENO, options)
}

#[cfg(not(unix))]
fn read_stdin_line_real_with_options(options: &ReadLineOptions) -> ReadLineOutcome {
    if !options.echo || options.raw {
        return ReadLineOutcome::Error(
            "std/io.read_line echo=false/raw=true is only implemented on Unix hosts".to_string(),
        );
    }
    if options.timeout_ms.is_some() {
        return ReadLineOutcome::Error(
            "std/io.read_line timeout_ms is only implemented on Unix hosts".to_string(),
        );
    }
    match read_stdin_line_real() {
        Some(line) => ReadLineOutcome::Ok(normalize_read_line_value(line, options.trim)),
        None => ReadLineOutcome::Eof,
    }
}

pub(crate) fn is_tty_for(stream: &str) -> bool {
    let mocked = TTY_MOCK.with(|t| {
        let mock = *t.borrow();
        match stream {
            "stdin" => mock.stdin,
            "stdout" => mock.stdout,
            "stderr" => mock.stderr,
            _ => None,
        }
    });
    if let Some(v) = mocked {
        return v;
    }
    match stream {
        "stdin" => std::io::stdin().is_terminal(),
        "stdout" => std::io::stdout().is_terminal(),
        "stderr" => std::io::stderr().is_terminal(),
        _ => false,
    }
}

fn ansi_enabled_for_stream(stream: &str) -> bool {
    let mode = COLOR_MODE.with(|m| *m.borrow());
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => {
            if std::env::var_os("FORCE_COLOR").is_some() {
                return true;
            }
            if std::env::var_os("NO_COLOR").is_some() {
                return false;
            }
            is_tty_for(stream)
        }
    }
}

pub(crate) fn register_io_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "log(message: any) -> nil",
    category = "io",
    doc = "Write a Harn-prefixed message to stdout."
)]
fn log_builtin(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    let msg = args.first().map(|a| a.display()).unwrap_or_default();
    write_stdout(out, &format!("[harn] {msg}\n"));
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "color(text: any, color: string) -> string",
    category = "io",
    doc = "Apply an ANSI foreground color when color output is enabled."
)]
fn color_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    let name = args.get(1).map(|a| a.display()).unwrap_or_default();
    if !ansi_enabled_for_stream("stdout") {
        return Ok(VmValue::String(arcstr::ArcStr::from(text)));
    }
    Ok(VmValue::String(arcstr::ArcStr::from(ansi_colorize(
        &text, &name,
    ))))
}

#[harn_builtin(
    sig = "bold(text: any) -> string",
    category = "io",
    doc = "Apply ANSI bold styling when color output is enabled."
)]
fn bold_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    if !ansi_enabled_for_stream("stdout") {
        return Ok(VmValue::String(arcstr::ArcStr::from(text)));
    }
    Ok(VmValue::String(arcstr::ArcStr::from(format!(
        "\u{1b}[1m{text}\u{1b}[0m"
    ))))
}

#[harn_builtin(
    sig = "dim(text: any) -> string",
    category = "io",
    doc = "Apply ANSI dim styling when color output is enabled."
)]
fn dim_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    if !ansi_enabled_for_stream("stdout") {
        return Ok(VmValue::String(arcstr::ArcStr::from(text)));
    }
    Ok(VmValue::String(arcstr::ArcStr::from(format!(
        "\u{1b}[2m{text}\u{1b}[0m"
    ))))
}

#[harn_builtin(
    sig = "set_color_mode(mode: string) -> nil",
    category = "io",
    doc = "Set ANSI color handling to auto, always, or never."
)]
fn set_color_mode_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mode = args.first().map(|a| a.display()).unwrap_or_default();
    let parsed = match mode.as_str() {
        "auto" => ColorMode::Auto,
        "always" => ColorMode::Always,
        "never" => ColorMode::Never,
        other => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                "set_color_mode: invalid mode '{other}'. Expected 'auto', 'always', or 'never'."
            ),
            ))));
        }
    };
    COLOR_MODE.with(|m| *m.borrow_mut() = parsed);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "__ansi_enabled(stream?: string) -> bool",
    category = "io",
    doc = "Return whether ANSI styling is enabled for stdin, stdout, or stderr."
)]
fn ansi_enabled_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let stream = args
        .first()
        .map(|a| a.display())
        .unwrap_or_else(|| "stdout".to_string());
    match stream.as_str() {
        "stdin" | "stdout" | "stderr" => Ok(VmValue::Bool(ansi_enabled_for_stream(&stream))),
        other => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!(
            "__ansi_enabled: invalid stream '{other}'. Expected 'stdin', 'stdout', or 'stderr'."
        ),
        )))),
    }
}

#[harn_builtin(
    sig = "read_stdin() -> string",
    category = "io",
    doc = "Read all remaining stdin as a string."
)]
fn read_stdin_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    // Drain any remaining mocked stdin first.
    let mocked = STDIN_MOCK.with(|s| s.borrow_mut().take());
    if let Some(buf) = mocked {
        // After read_stdin, future read_line calls return nil because stdin is consumed.
        STDIN_LINES.with(|lines| *lines.borrow_mut() = Some(VecDeque::new()));
        return Ok(VmValue::String(arcstr::ArcStr::from(buf)));
    }
    match read_stdin_all_real() {
        Some(s) => Ok(VmValue::String(arcstr::ArcStr::from(s))),
        None => Ok(VmValue::Nil),
    }
}

pub(crate) fn read_line_legacy_value() -> VmValue {
    let options = ReadLineOptions {
        trim: false,
        ..ReadLineOptions::default()
    };
    match read_line_from_mock_or_real(&options) {
        ReadLineOutcome::Ok(line) => VmValue::String(arcstr::ArcStr::from(line)),
        ReadLineOutcome::Eof => VmValue::Nil,
        #[cfg(unix)]
        ReadLineOutcome::Timeout => VmValue::Nil,
        #[cfg(unix)]
        ReadLineOutcome::Interrupt => VmValue::Nil,
        ReadLineOutcome::Error(_) => VmValue::Nil,
    }
}

pub(crate) fn read_line_structured_value(args: &[VmValue]) -> Result<VmValue, VmError> {
    let options = parse_read_line_options(args)?;
    Ok(read_line_result(read_line_from_mock_or_real(&options)))
}

#[harn_builtin(
    sig = "__io_read_line(options?: any) -> dict",
    category = "io",
    doc = "Read one line from stdin with structured status metadata."
)]
fn io_read_line_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    read_line_structured_value(args)
}

#[harn_builtin(
    sig = "__io_write_stderr(message: any) -> nil",
    category = "io",
    doc = "Write text to stderr without appending a newline."
)]
fn io_write_stderr_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let msg = args.first().map(|a| a.display()).unwrap_or_default();
    write_stderr(&msg);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "__io_write_stdout(message: any) -> nil",
    category = "io",
    doc = "Write text to stdout without appending a newline."
)]
fn io_write_stdout_builtin(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    let msg = args.first().map(|a| a.display()).unwrap_or_default();
    write_stdout(out, &msg);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "__io_print(...args: any) -> nil",
    category = "io",
    doc = "Internal compatibility bridge for stdout without newline."
)]
fn io_print_builtin(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    let msg = args.first().map(|a| a.display()).unwrap_or_default();
    write_stdout(out, &msg);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "__io_println(...args: any) -> nil",
    category = "io",
    doc = "Internal compatibility bridge for stdout with newline."
)]
fn io_println_builtin(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    let msg = args.first().map(|a| a.display()).unwrap_or_default();
    write_stdout(out, &format!("{msg}\n"));
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "__io_eprint(message: any) -> nil",
    category = "io",
    doc = "Internal compatibility bridge for stderr without newline."
)]
fn io_eprint_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let msg = args.first().map(|a| a.display()).unwrap_or_default();
    write_stderr(&msg);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "__io_eprintln(message: any) -> nil",
    category = "io",
    doc = "Internal compatibility bridge for stderr with newline."
)]
fn io_eprintln_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let msg = args.first().map(|a| a.display()).unwrap_or_default();
    write_stderr(&format!("{msg}\n"));
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "is_stdin_tty() -> bool",
    category = "io",
    doc = "Return whether stdin is attached to a terminal."
)]
fn is_stdin_tty_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(is_tty_for("stdin")))
}

#[harn_builtin(
    sig = "is_stdout_tty() -> bool",
    category = "io",
    doc = "Return whether stdout is attached to a terminal."
)]
fn is_stdout_tty_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(is_tty_for("stdout")))
}

#[harn_builtin(
    sig = "is_stderr_tty() -> bool",
    category = "io",
    doc = "Return whether stderr is attached to a terminal."
)]
fn is_stderr_tty_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(is_tty_for("stderr")))
}

#[harn_builtin(
    sig = "mock_stdin(text: string) -> nil",
    category = "io",
    doc = "Install mocked stdin text for tests."
)]
fn mock_stdin_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    STDIN_MOCK.with(|s| *s.borrow_mut() = Some(text));
    STDIN_LINES.with(|s| *s.borrow_mut() = None);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "unmock_stdin() -> nil",
    category = "io",
    doc = "Clear mocked stdin text and line state."
)]
fn unmock_stdin_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    STDIN_MOCK.with(|s| *s.borrow_mut() = None);
    STDIN_LINES.with(|s| *s.borrow_mut() = None);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "mock_tty(stream: string, is_tty: bool) -> nil",
    category = "io",
    doc = "Override terminal detection for stdin, stdout, or stderr."
)]
fn mock_tty_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let stream = args.first().map(|a| a.display()).unwrap_or_default();
    let is_tty = matches!(args.get(1), Some(VmValue::Bool(true)));
    TTY_MOCK.with(|t| {
        let mut mock = t.borrow_mut();
        match stream.as_str() {
            "stdin" => mock.stdin = Some(is_tty),
            "stdout" => mock.stdout = Some(is_tty),
            "stderr" => mock.stderr = Some(is_tty),
            other => {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    format!(
                    "mock_tty: invalid stream '{other}'. Expected 'stdin', 'stdout', or 'stderr'."
                ),
                ))));
            }
        }
        Ok(VmValue::Nil)
    })
}

#[harn_builtin(
    sig = "unmock_tty() -> nil",
    category = "io",
    doc = "Clear terminal detection overrides."
)]
fn unmock_tty_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    TTY_MOCK.with(|t| *t.borrow_mut() = TtyMock::default());
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "capture_stderr_start() -> nil",
    category = "io",
    doc = "Start capturing stderr into an in-memory buffer."
)]
fn capture_stderr_start_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    STDERR_CAPTURING.with(|c| *c.borrow_mut() = true);
    STDERR_BUFFER.with(|s| s.borrow_mut().clear());
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "capture_stderr_take() -> string",
    category = "io",
    doc = "Stop stderr capture and return the buffered text."
)]
fn capture_stderr_take_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let buf = STDERR_BUFFER.with(|s| std::mem::take(&mut *s.borrow_mut()));
    STDERR_CAPTURING.with(|c| *c.borrow_mut() = false);
    Ok(VmValue::String(arcstr::ArcStr::from(buf)))
}

#[harn_builtin(
    sig = "uuid() -> string",
    category = "io",
    doc = "Generate a random version 4 UUID."
)]
fn uuid_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::String(arcstr::ArcStr::from(
        uuid::Uuid::new_v4().to_string(),
    )))
}

#[harn_builtin(
    sig = "uuid_parse(value: any) -> string",
    category = "io",
    doc = "Parse and normalize a UUID string, or return nil."
)]
fn uuid_parse_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let raw = args.first().map(|a| a.display()).unwrap_or_default();
    match uuid::Uuid::parse_str(&raw) {
        Ok(uuid) => Ok(VmValue::String(arcstr::ArcStr::from(uuid.to_string()))),
        Err(_) => Ok(VmValue::Nil),
    }
}

#[harn_builtin(
    sig = "uuid_v7() -> string",
    category = "io",
    doc = "Generate a time-ordered version 7 UUID."
)]
fn uuid_v7_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::String(arcstr::ArcStr::from(
        uuid::Uuid::now_v7().to_string(),
    )))
}

#[harn_builtin(
    sig = "uuid_v5(namespace: string, name: string) -> string",
    category = "io",
    doc = "Generate a deterministic version 5 UUID."
)]
fn uuid_v5_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() < 2 {
        return Err(VmError::Runtime(
            "uuid_v5(namespace, name): requires namespace and name".to_string(),
        ));
    }
    let namespace_raw = args[0].display();
    let namespace = uuid_v5_namespace(&namespace_raw).ok_or_else(|| {
        VmError::Runtime("uuid_v5: namespace must be a UUID or one of dns/url/oid/x500".to_string())
    })?;
    let name = args[1].display();
    Ok(VmValue::String(arcstr::ArcStr::from(
        uuid::Uuid::new_v5(&namespace, name.as_bytes()).to_string(),
    )))
}

#[harn_builtin(
    sig = "uuid_nil() -> string",
    category = "io",
    doc = "Return the nil UUID."
)]
fn uuid_nil_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::String(arcstr::ArcStr::from(
        uuid::Uuid::nil().to_string(),
    )))
}

pub(crate) fn prompt_user_value(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    let msg = args.first().map(|a| a.display()).unwrap_or_default();
    write_stdout(out, &msg);
    let options = ReadLineOptions {
        trim: false,
        ..ReadLineOptions::default()
    };
    match read_line_from_mock_or_real(&options) {
        ReadLineOutcome::Ok(line) => Ok(VmValue::String(arcstr::ArcStr::from(
            line.trim_end().to_string(),
        ))),
        ReadLineOutcome::Eof => Ok(VmValue::Nil),
        #[cfg(unix)]
        ReadLineOutcome::Timeout => Ok(VmValue::Nil),
        #[cfg(unix)]
        ReadLineOutcome::Interrupt => Ok(VmValue::Nil),
        ReadLineOutcome::Error(_) => Ok(VmValue::Nil),
    }
}

pub(crate) fn read_password_legacy_value(prompt: &str) -> Result<VmValue, VmError> {
    let options = ReadLineOptions {
        prompt: prompt.to_string(),
        trim: false,
        echo: false,
        ..ReadLineOptions::default()
    };
    match read_line_from_mock_or_real(&options) {
        ReadLineOutcome::Ok(line) => Ok(VmValue::String(arcstr::ArcStr::from(line))),
        ReadLineOutcome::Eof => Err(VmError::Runtime(
            "HarnessTerm.read_password: stdin reached EOF".to_string(),
        )),
        #[cfg(unix)]
        ReadLineOutcome::Timeout => Err(VmError::Runtime(
            "HarnessTerm.read_password: stdin read timed out".to_string(),
        )),
        #[cfg(unix)]
        ReadLineOutcome::Interrupt => Err(VmError::Runtime(
            "HarnessTerm.read_password: stdin read was interrupted".to_string(),
        )),
        ReadLineOutcome::Error(error) => Err(VmError::Runtime(format!(
            "HarnessTerm.read_password: {error}"
        ))),
    }
}

#[harn_builtin(
    sig = "log_debug(message: any, fields?: dict) -> nil",
    category = "io",
    doc = "Write a structured debug log line."
)]
fn log_debug_builtin(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    vm_write_log("debug", 0, args, out);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "log_info(message: any, fields?: dict) -> nil",
    category = "io",
    doc = "Write a structured info log line."
)]
fn log_info_builtin(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    vm_write_log("info", 1, args, out);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "log_warn(message: any, fields?: dict) -> nil",
    category = "io",
    doc = "Write a structured warning log line."
)]
fn log_warn_builtin(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    vm_write_log("warn", 2, args, out);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "log_error(message: any, fields?: dict) -> nil",
    category = "io",
    doc = "Write a structured error log line."
)]
fn log_error_builtin(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    vm_write_log("error", 3, args, out);
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "log_set_level(level: string) -> nil",
    category = "io",
    doc = "Set the minimum structured log level."
)]
fn log_set_level_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let level_str = args.first().map(|a| a.display()).unwrap_or_default();
    match super::logging::vm_level_to_u8(&level_str) {
        Some(n) => {
            VM_MIN_LOG_LEVEL.store(n, Ordering::Relaxed);
            Ok(VmValue::Nil)
        }
        None => Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!(
                "log_set_level: invalid level '{level_str}'. Expected debug, info, warn, or error"
            ),
        )))),
    }
}

#[harn_builtin(
    sig = "progress(phase: string, message: string, progress_or_options?: any, total?: int) -> nil",
    category = "io",
    doc = "Write a human-readable progress log line."
)]
fn progress_builtin(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    write_stdout(out, &render_progress_line(args));
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "log_json(key: string, value?: any) -> nil",
    category = "io",
    doc = "Write a structured JSON log line."
)]
fn log_json_builtin(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    let key = args.first().map(|a| a.display()).unwrap_or_default();
    let value = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let json_val = super::logging::vm_value_to_json_fragment(&value);
    let ts = super::logging::vm_format_timestamp_utc();
    let line = format!(
        "{{\"ts\":{},\"key\":{},\"value\":{}}}\n",
        vm_escape_json_str_quoted(&ts),
        vm_escape_json_str_quoted(&key),
        json_val,
    );
    write_stdout(out, &line);
    Ok(VmValue::Nil)
}

fn uuid_v5_namespace(raw: &str) -> Option<uuid::Uuid> {
    match raw.to_ascii_lowercase().as_str() {
        "dns" | "namespace_dns" => Some(uuid::Uuid::NAMESPACE_DNS),
        "url" | "namespace_url" => Some(uuid::Uuid::NAMESPACE_URL),
        "oid" | "namespace_oid" => Some(uuid::Uuid::NAMESPACE_OID),
        "x500" | "namespace_x500" => Some(uuid::Uuid::NAMESPACE_X500),
        _ => uuid::Uuid::parse_str(raw).ok(),
    }
}

fn render_progress_line(args: &[VmValue]) -> String {
    let phase = args.first().map(|a| a.display()).unwrap_or_default();
    let message = args.get(1).map(|a| a.display()).unwrap_or_default();

    if let Some(options) = args.get(2).and_then(|arg| arg.as_dict()) {
        if let Some(mode) = progress_dict_str(options, "mode") {
            match mode {
                "spinner" => {
                    let step = progress_dict_int(options, "step")
                        .or_else(|| progress_dict_int(options, "current"))
                        .unwrap_or(0);
                    let frame = spinner_frame(step);
                    return format!("[{phase}] {frame} {message}\n");
                }
                "bar" => {
                    let current = progress_dict_int(options, "current").unwrap_or(0);
                    let total = progress_dict_int(options, "total").unwrap_or(0);
                    let width = progress_dict_int(options, "width")
                        .unwrap_or(10)
                        .clamp(3, 40) as usize;
                    let bar = render_progress_bar(current, total, width);
                    return format!("[{phase}] {bar} {message} ({current}/{total})\n");
                }
                _ => {}
            }
        }
    }

    let progress = args.get(2).and_then(|a| a.as_int());
    let total = args.get(3).and_then(|a| a.as_int());
    match (progress, total) {
        (Some(p), Some(t)) => format!("[{phase}] {message} ({p}/{t})\n"),
        (Some(p), None) => format!("[{phase}] {message} ({p}%)\n"),
        _ => format!("[{phase}] {message}\n"),
    }
}

fn progress_dict_int(options: &crate::value::DictMap, key: &str) -> Option<i64> {
    options.get(key).and_then(|value| value.as_int())
}

fn progress_dict_str<'a>(options: &'a crate::value::DictMap, key: &str) -> Option<&'a str> {
    match options.get(key) {
        Some(VmValue::String(value)) => Some(value.as_ref()),
        _ => None,
    }
}

fn spinner_frame(step: i64) -> &'static str {
    match step.rem_euclid(4) {
        0 => "|",
        1 => "/",
        2 => "-",
        _ => "\\",
    }
}

fn render_progress_bar(current: i64, total: i64, width: usize) -> String {
    if total <= 0 {
        return format!("[{}]", "-".repeat(width));
    }

    let clamped = current.clamp(0, total);
    let filled = ((clamped as f64 / total as f64) * width as f64).round() as usize;
    let filled = filled.min(width);
    let empty = width.saturating_sub(filled);
    format!("[{}{}]", "#".repeat(filled), "-".repeat(empty))
}

fn vm_write_log(level: &str, level_num: u8, args: &[VmValue], out: &mut String) {
    if level_num < VM_MIN_LOG_LEVEL.load(Ordering::Relaxed) {
        return;
    }
    let msg = args.first().map(|a| a.display()).unwrap_or_default();
    let fields = args.get(1).and_then(|v| {
        if let VmValue::Dict(d) = v {
            Some(&**d)
        } else {
            None
        }
    });
    let line = vm_build_log_line(level, &msg, fields);
    write_stdout(out, &line);
}

fn ansi_colorize(text: &str, name: &str) -> String {
    let code = match name {
        "black" => "30",
        "red" => "31",
        "green" => "32",
        "yellow" => "33",
        "blue" => "34",
        "magenta" => "35",
        "cyan" => "36",
        "white" => "37",
        "bright_black" | "gray" | "grey" => "90",
        "bright_red" => "91",
        "bright_green" => "92",
        "bright_yellow" => "93",
        "bright_blue" => "94",
        "bright_magenta" => "95",
        "bright_cyan" => "96",
        "bright_white" => "97",
        _ => return text.to_string(),
    };
    format!("\u{1b}[{code}m{text}\u{1b}[0m")
}

#[cfg(test)]
mod tests {
    use crate::value::VmDictExt;
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicBool, Ordering};
    #[cfg(unix)]
    use std::sync::Arc;
    #[cfg(unix)]
    use std::time::Instant;

    use crate::value::VmValue;

    use super::{
        render_progress_bar, render_progress_line, reset_io_state, set_stdout_passthrough,
        spinner_frame, stdout_passthrough_enabled,
    };
    #[cfg(unix)]
    use super::{ReadLineOptions, ReadLineOutcome};

    #[test]
    fn stdout_passthrough_state_toggles() {
        reset_io_state();

        assert!(!stdout_passthrough_enabled());
        assert!(!set_stdout_passthrough(true));
        assert!(stdout_passthrough_enabled());

        assert!(set_stdout_passthrough(false));
        assert!(!stdout_passthrough_enabled());
    }

    #[test]
    fn progress_bar_mode_renders_hash_bar() {
        let mut options = BTreeMap::new();
        options.put_str("mode", "bar");
        options.insert("current".to_string(), VmValue::Int(3));
        options.insert("total".to_string(), VmValue::Int(5));
        options.insert("width".to_string(), VmValue::Int(10));

        let line = render_progress_line(&[
            VmValue::String(arcstr::ArcStr::from("build")),
            VmValue::String(arcstr::ArcStr::from("Compiling")),
            VmValue::dict(options),
        ]);

        assert_eq!(line, "[build] [######----] Compiling (3/5)\n");
    }

    #[test]
    fn progress_spinner_mode_uses_step_to_pick_frame() {
        let mut options = BTreeMap::new();
        options.put_str("mode", "spinner");
        options.insert("step".to_string(), VmValue::Int(2));

        let line = render_progress_line(&[
            VmValue::String(arcstr::ArcStr::from("sync")),
            VmValue::String(arcstr::ArcStr::from("Waiting")),
            VmValue::dict(options),
        ]);

        assert_eq!(line, "[sync] - Waiting\n");
        assert_eq!(spinner_frame(3), "\\");
    }

    #[test]
    fn progress_bar_falls_back_to_empty_bar_for_zero_total() {
        assert_eq!(render_progress_bar(2, 0, 5), "[-----]");
    }

    #[test]
    fn read_line_options_preserve_prompt_whitespace() {
        let mut options = BTreeMap::new();
        options.put_str("prompt", "  > ");
        options.insert("trim".to_string(), VmValue::Bool(false));

        let parsed = super::parse_read_line_options(&[VmValue::dict(options)]).unwrap();

        assert_eq!(parsed.prompt, "  > ");
        assert!(!parsed.trim);
    }

    #[test]
    fn read_line_options_reject_unknown_keys() {
        let mut options = BTreeMap::new();
        options.put_str("promtp", "> ");

        let err = super::parse_read_line_options(&[VmValue::dict(options)]).unwrap_err();

        match err {
            crate::value::VmError::Runtime(message) => assert!(message.contains("promtp")),
            other => panic!("expected Runtime error, got {other:?}"),
        }
    }

    #[cfg(unix)]
    struct FdGuard(libc::c_int);

    #[cfg(unix)]
    impl Drop for FdGuard {
        fn drop(&mut self) {
            let _ = unsafe { libc::close(self.0) };
        }
    }

    #[cfg(unix)]
    fn pipe_pair() -> (FdGuard, FdGuard) {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        (FdGuard(fds[0]), FdGuard(fds[1]))
    }

    #[cfg(unix)]
    #[test]
    fn read_line_from_fd_times_out_without_data() {
        let (read_fd, _write_fd) = pipe_pair();
        let outcome = super::read_line_from_fd_unix(
            read_fd.0,
            &ReadLineOptions {
                timeout_ms: Some(10),
                ..ReadLineOptions::default()
            },
        );

        assert_eq!(outcome, ReadLineOutcome::Timeout);
    }

    #[cfg(unix)]
    #[test]
    fn read_line_from_fd_observes_interrupt_without_stdin_activity() {
        let (read_fd, _write_fd) = pipe_pair();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_from_thread = Arc::clone(&cancel);
        let _guard = crate::op_interrupt::install(Some(cancel), None);
        let interrupter = std::thread::spawn(move || {
            // No fixed "wait for the reader to park" sleep: the reader re-checks
            // the cancel flag every `READ_LINE_INTERRUPT_POLL` heartbeat, so it
            // observes this store within one interval regardless of ordering.
            // A blind sleep would only add wall time and reintroduce a race.
            cancel_from_thread.store(true, Ordering::SeqCst);
        });

        let started = Instant::now();
        let outcome = super::read_line_from_fd_unix(read_fd.0, &ReadLineOptions::default());

        interrupter.join().expect("interrupter thread joins");
        assert_eq!(outcome, ReadLineOutcome::Interrupt);
        assert!(
            started.elapsed() < super::READ_LINE_INTERRUPT_POLL * 25,
            "interrupt heartbeat should wake idle read_line within a few poll \
             intervals, took {:?}",
            started.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_line_from_fd_honors_trim_option() {
        let (read_fd, write_fd) = pipe_pair();
        let payload = b"  alpha  \n";
        assert_eq!(
            unsafe { libc::write(write_fd.0, payload.as_ptr().cast(), payload.len()) },
            payload.len() as isize
        );
        let outcome = super::read_line_from_fd_unix(
            read_fd.0,
            &ReadLineOptions {
                timeout_ms: Some(100),
                trim: false,
                ..ReadLineOptions::default()
            },
        );

        assert_eq!(outcome, ReadLineOutcome::Ok("  alpha  ".to_string()));
    }
}
