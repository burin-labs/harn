use std::io::BufRead;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::logging::{vm_build_log_line, vm_escape_json_str_quoted, VM_MIN_LOG_LEVEL};

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
        Ok(VmValue::String(Rc::from(ansi_colorize(&text, &name))))
    });

    vm.register_builtin("bold", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(format!(
            "\u{1b}[1m{text}\u{1b}[0m"
        ))))
    });

    vm.register_builtin("dim", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(format!(
            "\u{1b}[2m{text}\u{1b}[0m"
        ))))
    });

    vm.register_builtin("uuid", |_args, _out| {
        Ok(VmValue::String(Rc::from(uuid::Uuid::new_v4().to_string())))
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
