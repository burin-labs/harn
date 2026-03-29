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

    // --- Logging builtins ---

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

    // progress(phase, message, data?) — standalone mode writes structured log line.
    // In bridge/ACP mode, this is overridden to emit structured notifications.
    vm.register_builtin("progress", |args, out| {
        let phase = args.first().map(|a| a.display()).unwrap_or_default();
        let message = args.get(1).map(|a| a.display()).unwrap_or_default();
        out.push_str(&format!("[{phase}] {message}\n"));
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
