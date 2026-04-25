use std::cell::RefCell;
use std::io::{BufRead, IsTerminal, Read, Write};
use std::rc::Rc;
use std::sync::atomic::Ordering;

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

thread_local! {
    static STDIN_MOCK: RefCell<Option<String>> = const { RefCell::new(None) };
    static STDIN_LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    static STDERR_BUFFER: RefCell<String> = const { RefCell::new(String::new()) };
    static STDERR_CAPTURING: RefCell<bool> = const { RefCell::new(false) };
    static TTY_MOCK: RefCell<TtyMock> = const { RefCell::new(TtyMock { stdin: None, stdout: None, stderr: None }) };
    static COLOR_MODE: RefCell<ColorMode> = const { RefCell::new(ColorMode::Auto) };
}

/// Reset all io thread-local state for test isolation.
pub(crate) fn reset_io_state() {
    STDIN_MOCK.with(|s| *s.borrow_mut() = None);
    STDIN_LINES.with(|s| *s.borrow_mut() = None);
    STDERR_BUFFER.with(|s| s.borrow_mut().clear());
    STDERR_CAPTURING.with(|s| *s.borrow_mut() = false);
    TTY_MOCK.with(|t| *t.borrow_mut() = TtyMock::default());
    COLOR_MODE.with(|m| *m.borrow_mut() = ColorMode::Auto);
}

/// Drain and return the buffered stderr output. The CLI flushes this to
/// the real stderr at the end of execution.
pub fn take_stderr_buffer() -> String {
    STDERR_BUFFER.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

fn write_stderr(line: &str) {
    let capturing = STDERR_CAPTURING.with(|c| *c.borrow());
    if capturing {
        STDERR_BUFFER.with(|s| s.borrow_mut().push_str(line));
    } else {
        // Pass through directly; the CLI flushes at end too if anything
        // accumulated before capture toggled, but normally nothing does.
        let _ = std::io::stderr().write_all(line.as_bytes());
    }
}

fn read_stdin_all_real() -> Option<String> {
    let mut buf = String::new();
    if std::io::stdin().lock().read_to_string(&mut buf).is_ok() {
        Some(buf)
    } else {
        None
    }
}

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

fn pop_mock_line() -> Option<String> {
    STDIN_LINES.with(|lines| {
        let mut borrow = lines.borrow_mut();
        let queue = borrow.as_mut()?;
        if queue.is_empty() {
            None
        } else {
            Some(queue.remove(0))
        }
    })
}

fn is_tty_for(stream: &str) -> bool {
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
    vm.register_builtin("log", |args, out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        out.push_str(&format!("[harn] {msg}\n"));
        Ok(VmValue::Nil)
    });
    vm.register_builtin("print", |args, out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        out.push_str(&msg);
        Ok(VmValue::Nil)
    });
    vm.register_builtin("println", |args, out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        out.push_str(&format!("{msg}\n"));
        Ok(VmValue::Nil)
    });

    vm.register_builtin("color", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        let name = args.get(1).map(|a| a.display()).unwrap_or_default();
        if !ansi_enabled_for_stream("stdout") {
            return Ok(VmValue::String(Rc::from(text)));
        }
        Ok(VmValue::String(Rc::from(ansi_colorize(&text, &name))))
    });

    vm.register_builtin("bold", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        if !ansi_enabled_for_stream("stdout") {
            return Ok(VmValue::String(Rc::from(text)));
        }
        Ok(VmValue::String(Rc::from(format!(
            "\u{1b}[1m{text}\u{1b}[0m"
        ))))
    });

    vm.register_builtin("dim", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        if !ansi_enabled_for_stream("stdout") {
            return Ok(VmValue::String(Rc::from(text)));
        }
        Ok(VmValue::String(Rc::from(format!(
            "\u{1b}[2m{text}\u{1b}[0m"
        ))))
    });

    vm.register_builtin("set_color_mode", |args, _out| {
        let mode = args.first().map(|a| a.display()).unwrap_or_default();
        let parsed = match mode.as_str() {
            "auto" => ColorMode::Auto,
            "always" => ColorMode::Always,
            "never" => ColorMode::Never,
            other => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "set_color_mode: invalid mode '{other}'. Expected 'auto', 'always', or 'never'."
                )))));
            }
        };
        COLOR_MODE.with(|m| *m.borrow_mut() = parsed);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("eprint", |args, _out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        write_stderr(&msg);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("eprintln", |args, _out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        write_stderr(&format!("{msg}\n"));
        Ok(VmValue::Nil)
    });

    vm.register_builtin("read_stdin", |_args, _out| {
        // Drain any remaining mocked stdin first.
        let mocked = STDIN_MOCK.with(|s| s.borrow_mut().take());
        if let Some(buf) = mocked {
            // After read_stdin, future read_line calls return nil — stdin is consumed.
            STDIN_LINES.with(|lines| *lines.borrow_mut() = Some(Vec::new()));
            return Ok(VmValue::String(Rc::from(buf)));
        }
        match read_stdin_all_real() {
            Some(s) => Ok(VmValue::String(Rc::from(s))),
            None => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("read_line", |_args, _out| {
        // Mock case: prefer the line queue, then split the bulk mock if present.
        if let Some(line) = pop_mock_line() {
            return Ok(VmValue::String(Rc::from(line)));
        }
        let bulk = STDIN_MOCK.with(|s| s.borrow_mut().take());
        if let Some(text) = bulk {
            let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
            // The trailing empty element from a final newline is not a line.
            if matches!(lines.last(), Some(l) if l.is_empty()) {
                lines.pop();
            }
            let first = if lines.is_empty() {
                None
            } else {
                Some(lines.remove(0))
            };
            STDIN_LINES.with(|q| *q.borrow_mut() = Some(lines));
            return Ok(first
                .map(|s| VmValue::String(Rc::from(s)))
                .unwrap_or(VmValue::Nil));
        }
        // Real stdin path. EOF or read error returns nil.
        match read_stdin_line_real() {
            Some(line) => Ok(VmValue::String(Rc::from(line))),
            None => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("is_stdin_tty", |_args, _out| {
        Ok(VmValue::Bool(is_tty_for("stdin")))
    });
    vm.register_builtin("is_stdout_tty", |_args, _out| {
        Ok(VmValue::Bool(is_tty_for("stdout")))
    });
    vm.register_builtin("is_stderr_tty", |_args, _out| {
        Ok(VmValue::Bool(is_tty_for("stderr")))
    });

    vm.register_builtin("mock_stdin", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        STDIN_MOCK.with(|s| *s.borrow_mut() = Some(text));
        STDIN_LINES.with(|s| *s.borrow_mut() = None);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("unmock_stdin", |_args, _out| {
        STDIN_MOCK.with(|s| *s.borrow_mut() = None);
        STDIN_LINES.with(|s| *s.borrow_mut() = None);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("mock_tty", |args, _out| {
        let stream = args.first().map(|a| a.display()).unwrap_or_default();
        let is_tty = matches!(args.get(1), Some(VmValue::Bool(true)));
        TTY_MOCK.with(|t| {
            let mut mock = t.borrow_mut();
            match stream.as_str() {
                "stdin" => mock.stdin = Some(is_tty),
                "stdout" => mock.stdout = Some(is_tty),
                "stderr" => mock.stderr = Some(is_tty),
                other => {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "mock_tty: invalid stream '{other}'. Expected 'stdin', 'stdout', or 'stderr'."
                    )))));
                }
            }
            Ok(VmValue::Nil)
        })
    });

    vm.register_builtin("unmock_tty", |_args, _out| {
        TTY_MOCK.with(|t| *t.borrow_mut() = TtyMock::default());
        Ok(VmValue::Nil)
    });

    vm.register_builtin("capture_stderr_start", |_args, _out| {
        STDERR_CAPTURING.with(|c| *c.borrow_mut() = true);
        STDERR_BUFFER.with(|s| s.borrow_mut().clear());
        Ok(VmValue::Nil)
    });

    vm.register_builtin("capture_stderr_take", |_args, _out| {
        let buf = STDERR_BUFFER.with(|s| std::mem::take(&mut *s.borrow_mut()));
        STDERR_CAPTURING.with(|c| *c.borrow_mut() = false);
        Ok(VmValue::String(Rc::from(buf)))
    });

    vm.register_builtin("uuid", |_args, _out| {
        Ok(VmValue::String(Rc::from(uuid::Uuid::new_v4().to_string())))
    });

    vm.register_builtin("uuid_parse", |args, _out| {
        let raw = args.first().map(|a| a.display()).unwrap_or_default();
        match uuid::Uuid::parse_str(&raw) {
            Ok(uuid) => Ok(VmValue::String(Rc::from(uuid.to_string()))),
            Err(_) => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("uuid_v7", |_args, _out| {
        Ok(VmValue::String(Rc::from(uuid::Uuid::now_v7().to_string())))
    });

    vm.register_builtin("uuid_v5", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Runtime(
                "uuid_v5(namespace, name): requires namespace and name".to_string(),
            ));
        }
        let namespace_raw = args[0].display();
        let namespace = uuid_v5_namespace(&namespace_raw).ok_or_else(|| {
            VmError::Runtime(
                "uuid_v5: namespace must be a UUID or one of dns/url/oid/x500".to_string(),
            )
        })?;
        let name = args[1].display();
        Ok(VmValue::String(Rc::from(
            uuid::Uuid::new_v5(&namespace, name.as_bytes()).to_string(),
        )))
    });

    vm.register_builtin("uuid_nil", |_args, _out| {
        Ok(VmValue::String(Rc::from(uuid::Uuid::nil().to_string())))
    });

    vm.register_builtin("prompt_user", |args, out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        out.push_str(&msg);
        let mut input = String::new();
        if std::io::stdin().lock().read_line(&mut input).is_ok() {
            Ok(VmValue::String(Rc::from(input.trim_end())))
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("log_debug", |args, out| {
        vm_write_log("debug", 0, args, out);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("log_info", |args, out| {
        vm_write_log("info", 1, args, out);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("log_warn", |args, out| {
        vm_write_log("warn", 2, args, out);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("log_error", |args, out| {
        vm_write_log("error", 3, args, out);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("log_set_level", |args, _out| {
        let level_str = args.first().map(|a| a.display()).unwrap_or_default();
        match super::logging::vm_level_to_u8(&level_str) {
            Some(n) => {
                VM_MIN_LOG_LEVEL.store(n, Ordering::Relaxed);
                Ok(VmValue::Nil)
            }
            None => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "log_set_level: invalid level '{}'. Expected debug, info, warn, or error",
                level_str
            ))))),
        }
    });

    // Standalone mode writes a structured log line; bridge/ACP mode overrides
    // this to emit structured notifications.
    vm.register_builtin("progress", |args, out| {
        out.push_str(&render_progress_line(args));
        Ok(VmValue::Nil)
    });

    vm.register_builtin("log_json", |args, out| {
        let key = args.first().map(|a| a.display()).unwrap_or_default();
        let value = args.get(1).cloned().unwrap_or(VmValue::Nil);
        let json_val = super::logging::vm_value_to_json_fragment(&value);
        let ts = super::logging::vm_format_timestamp_utc();
        out.push_str(&format!(
            "{{\"ts\":{},\"key\":{},\"value\":{}}}\n",
            vm_escape_json_str_quoted(&ts),
            vm_escape_json_str_quoted(&key),
            json_val,
        ));
        Ok(VmValue::Nil)
    });
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

fn progress_dict_int(
    options: &std::collections::BTreeMap<String, VmValue>,
    key: &str,
) -> Option<i64> {
    options.get(key).and_then(|value| value.as_int())
}

fn progress_dict_str<'a>(
    options: &'a std::collections::BTreeMap<String, VmValue>,
    key: &str,
) -> Option<&'a str> {
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
    out.push_str(&line);
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
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use crate::value::VmValue;

    use super::{render_progress_bar, render_progress_line, spinner_frame};

    #[test]
    fn progress_bar_mode_renders_hash_bar() {
        let mut options = BTreeMap::new();
        options.insert("mode".to_string(), VmValue::String(Rc::from("bar")));
        options.insert("current".to_string(), VmValue::Int(3));
        options.insert("total".to_string(), VmValue::Int(5));
        options.insert("width".to_string(), VmValue::Int(10));

        let line = render_progress_line(&[
            VmValue::String(Rc::from("build")),
            VmValue::String(Rc::from("Compiling")),
            VmValue::Dict(Rc::new(options)),
        ]);

        assert_eq!(line, "[build] [######----] Compiling (3/5)\n");
    }

    #[test]
    fn progress_spinner_mode_uses_step_to_pick_frame() {
        let mut options = BTreeMap::new();
        options.insert("mode".to_string(), VmValue::String(Rc::from("spinner")));
        options.insert("step".to_string(), VmValue::Int(2));

        let line = render_progress_line(&[
            VmValue::String(Rc::from("sync")),
            VmValue::String(Rc::from("Waiting")),
            VmValue::Dict(Rc::new(options)),
        ]);

        assert_eq!(line, "[sync] - Waiting\n");
        assert_eq!(spinner_frame(3), "\\");
    }

    #[test]
    fn progress_bar_falls_back_to_empty_bar_for_zero_total() {
        assert_eq!(render_progress_bar(2, 0, 5), "[-----]");
    }
}
