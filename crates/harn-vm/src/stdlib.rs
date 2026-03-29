use std::collections::BTreeMap;
use std::io::BufRead;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::sync::Arc;

use crate::value::{values_equal, VmAtomicHandle, VmChannelHandle, VmError, VmValue};
use crate::vm::Vm;

use crate::http::register_http_builtins;
use crate::llm::register_llm_builtins;
use crate::mcp::register_mcp_builtins;

/// Build a select result dict with the given index, value, and channel name.
fn select_result(index: usize, value: VmValue, channel_name: &str) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("index".to_string(), VmValue::Int(index as i64));
    result.insert("value".to_string(), value);
    result.insert(
        "channel".to_string(),
        VmValue::String(Rc::from(channel_name)),
    );
    VmValue::Dict(Rc::new(result))
}

/// Build a select result dict indicating no channel was ready (index = -1).
fn select_none() -> VmValue {
    let mut result = BTreeMap::new();
    result.insert("index".to_string(), VmValue::Int(-1));
    result.insert("value".to_string(), VmValue::Nil);
    result.insert("channel".to_string(), VmValue::Nil);
    VmValue::Dict(Rc::new(result))
}

/// Try to receive from a list of channels (non-blocking).
/// Returns Some((index, value, channel_name)) if a message was received,
/// or None. Sets all_closed to true if all channels are disconnected.
fn try_poll_channels(channels: &[VmValue]) -> (Option<(usize, VmValue, String)>, bool) {
    let mut all_closed = true;
    for (i, ch_val) in channels.iter().enumerate() {
        if let VmValue::Channel(ch) = ch_val {
            if let Ok(mut rx) = ch.receiver.try_lock() {
                match rx.try_recv() {
                    Ok(val) => return (Some((i, val, ch.name.clone())), false),
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                        all_closed = false;
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {}
                }
            } else {
                all_closed = false;
            }
        }
    }
    (None, all_closed)
}

/// Register standard builtins on a VM.
pub fn register_vm_stdlib(vm: &mut Vm) {
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
    vm.register_builtin("type_of", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::String(Rc::from(val.type_name())))
    });
    vm.register_builtin("to_string", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::String(Rc::from(val.display())))
    });
    vm.register_builtin("to_int", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        match val {
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            VmValue::Float(n) => Ok(VmValue::Int(*n as i64)),
            VmValue::String(s) => Ok(s.parse::<i64>().map(VmValue::Int).unwrap_or(VmValue::Nil)),
            _ => Ok(VmValue::Nil),
        }
    });
    vm.register_builtin("to_float", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        match val {
            VmValue::Float(n) => Ok(VmValue::Float(*n)),
            VmValue::Int(n) => Ok(VmValue::Float(*n as f64)),
            VmValue::String(s) => Ok(s.parse::<f64>().map(VmValue::Float).unwrap_or(VmValue::Nil)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("json_stringify", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::String(Rc::from(vm_value_to_json(val))))
    });

    vm.register_builtin("json_parse", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(jv) => Ok(json_to_vm_value(&jv)),
            Err(e) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "JSON parse error: {e}"
            ))))),
        }
    });

    vm.register_builtin("env", |args, _out| {
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        match std::env::var(&name) {
            Ok(val) => Ok(VmValue::String(Rc::from(val))),
            Err(_) => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("timestamp", |_args, _out| {
        use std::time::{SystemTime, UNIX_EPOCH};
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        Ok(VmValue::Float(secs))
    });

    vm.register_builtin("read_file", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        match std::fs::read_to_string(&path) {
            Ok(content) => Ok(VmValue::String(Rc::from(content))),
            Err(e) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to read file {path}: {e}"
            ))))),
        }
    });

    vm.register_builtin("write_file", |args, _out| {
        if args.len() >= 2 {
            let path = args[0].display();
            let content = args[1].display();
            std::fs::write(&path, &content).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to write file {path}: {e}"
                ))))
            })?;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("exit", |args, _out| {
        let code = args.first().and_then(|a| a.as_int()).unwrap_or(0);
        std::process::exit(code as i32);
    });

    vm.register_builtin("regex_match", |args, _out| {
        if args.len() >= 2 {
            let pattern = args[0].display();
            let text = args[1].display();
            let re = regex::Regex::new(&pattern).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!("Invalid regex: {e}"))))
            })?;
            let matches: Vec<VmValue> = re
                .find_iter(&text)
                .map(|m| VmValue::String(Rc::from(m.as_str())))
                .collect();
            if matches.is_empty() {
                return Ok(VmValue::Nil);
            }
            return Ok(VmValue::List(Rc::new(matches)));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("regex_replace", |args, _out| {
        if args.len() >= 3 {
            let pattern = args[0].display();
            let replacement = args[1].display();
            let text = args[2].display();
            let re = regex::Regex::new(&pattern).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!("Invalid regex: {e}"))))
            })?;
            return Ok(VmValue::String(Rc::from(
                re.replace_all(&text, replacement.as_str()).into_owned(),
            )));
        }
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

    // --- Math builtins ---

    vm.register_builtin("abs", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Int(n) => Ok(VmValue::Int(n.wrapping_abs())),
            VmValue::Float(n) => Ok(VmValue::Float(n.abs())),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("min", |args, _out| {
        if args.len() >= 2 {
            match (&args[0], &args[1]) {
                (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(*x.min(y))),
                (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x.min(*y))),
                (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float((*x as f64).min(*y))),
                (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x.min(*y as f64))),
                _ => Ok(VmValue::Nil),
            }
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("max", |args, _out| {
        if args.len() >= 2 {
            match (&args[0], &args[1]) {
                (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(*x.max(y))),
                (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x.max(*y))),
                (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float((*x as f64).max(*y))),
                (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x.max(*y as f64))),
                _ => Ok(VmValue::Nil),
            }
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("floor", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Int(n.floor() as i64)),
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("ceil", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Int(n.ceil() as i64)),
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("round", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Int(n.round() as i64)),
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("sqrt", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Float(n.sqrt())),
            VmValue::Int(n) => Ok(VmValue::Float((*n as f64).sqrt())),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("pow", |args, _out| {
        if args.len() >= 2 {
            match (&args[0], &args[1]) {
                (VmValue::Int(base), VmValue::Int(exp)) => {
                    if *exp >= 0 && *exp <= u32::MAX as i64 {
                        Ok(VmValue::Int(base.wrapping_pow(*exp as u32)))
                    } else {
                        Ok(VmValue::Float((*base as f64).powf(*exp as f64)))
                    }
                }
                (VmValue::Float(base), VmValue::Int(exp)) => {
                    if *exp >= i32::MIN as i64 && *exp <= i32::MAX as i64 {
                        Ok(VmValue::Float(base.powi(*exp as i32)))
                    } else {
                        Ok(VmValue::Float(base.powf(*exp as f64)))
                    }
                }
                (VmValue::Int(base), VmValue::Float(exp)) => {
                    Ok(VmValue::Float((*base as f64).powf(*exp)))
                }
                (VmValue::Float(base), VmValue::Float(exp)) => Ok(VmValue::Float(base.powf(*exp))),
                _ => Ok(VmValue::Nil),
            }
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("random", |_args, _out| {
        use rand::Rng;
        let val: f64 = rand::thread_rng().gen();
        Ok(VmValue::Float(val))
    });

    vm.register_builtin("random_int", |args, _out| {
        use rand::Rng;
        if args.len() >= 2 {
            let min = args[0].as_int().unwrap_or(0);
            let max = args[1].as_int().unwrap_or(0);
            if min <= max {
                let val = rand::thread_rng().gen_range(min..=max);
                return Ok(VmValue::Int(val));
            }
        }
        Ok(VmValue::Nil)
    });

    // --- Assert builtins ---

    vm.register_builtin("assert", |args, _out| {
        let condition = args.first().unwrap_or(&VmValue::Nil);
        if !condition.is_truthy() {
            let msg = args
                .get(1)
                .map(|a| a.display())
                .unwrap_or_else(|| "Assertion failed".to_string());
            return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("assert_eq", |args, _out| {
        if args.len() >= 2 {
            if !values_equal(&args[0], &args[1]) {
                let msg = args.get(2).map(|a| a.display()).unwrap_or_else(|| {
                    format!(
                        "Assertion failed: expected {}, got {}",
                        args[1].display(),
                        args[0].display()
                    )
                });
                return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
            }
            Ok(VmValue::Nil)
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "assert_eq requires at least 2 arguments",
            ))))
        }
    });

    vm.register_builtin("assert_ne", |args, _out| {
        if args.len() >= 2 {
            if values_equal(&args[0], &args[1]) {
                let msg = args.get(2).map(|a| a.display()).unwrap_or_else(|| {
                    format!(
                        "Assertion failed: values should not be equal: {}",
                        args[0].display()
                    )
                });
                return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
            }
            Ok(VmValue::Nil)
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "assert_ne requires at least 2 arguments",
            ))))
        }
    });

    vm.register_builtin("__range__", |args, _out| {
        let start = args.first().and_then(|a| a.as_int()).unwrap_or(0);
        let end = args.get(1).and_then(|a| a.as_int()).unwrap_or(0);
        let inclusive = args.get(2).map(|a| a.is_truthy()).unwrap_or(false);
        let items: Vec<VmValue> = if inclusive {
            (start..=end).map(VmValue::Int).collect()
        } else {
            (start..end).map(VmValue::Int).collect()
        };
        Ok(VmValue::List(Rc::new(items)))
    });

    // =========================================================================
    // File system builtins
    // =========================================================================

    vm.register_builtin("file_exists", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::Bool(std::path::Path::new(&path).exists()))
    });

    vm.register_builtin("delete_file", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let p = std::path::Path::new(&path);
        if p.is_dir() {
            std::fs::remove_dir_all(&path).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to delete directory {path}: {e}"
                ))))
            })?;
        } else {
            std::fs::remove_file(&path).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to delete file {path}: {e}"
                ))))
            })?;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("append_file", |args, _out| {
        use std::io::Write;
        if args.len() >= 2 {
            let path = args[0].display();
            let content = args[1].display();
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)
                .map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "Failed to open file {path}: {e}"
                    ))))
                })?;
            file.write_all(content.as_bytes()).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to append to file {path}: {e}"
                ))))
            })?;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("list_dir", |args, _out| {
        let path = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| ".".to_string());
        let entries = std::fs::read_dir(&path).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to list directory {path}: {e}"
            ))))
        })?;
        let mut result = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|e| VmError::Thrown(VmValue::String(Rc::from(e.to_string()))))?;
            let name = entry.file_name().to_string_lossy().to_string();
            result.push(VmValue::String(Rc::from(name.as_str())));
        }
        result.sort_by_key(|a| a.display());
        Ok(VmValue::List(Rc::new(result)))
    });

    vm.register_builtin("mkdir", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        std::fs::create_dir_all(&path).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to create directory {path}: {e}"
            ))))
        })?;
        Ok(VmValue::Nil)
    });

    vm.register_builtin("path_join", |args, _out| {
        let mut path = std::path::PathBuf::new();
        for arg in args {
            path.push(arg.display());
        }
        Ok(VmValue::String(Rc::from(
            path.to_string_lossy().to_string().as_str(),
        )))
    });

    vm.register_builtin("copy_file", |args, _out| {
        if args.len() >= 2 {
            let src = args[0].display();
            let dst = args[1].display();
            std::fs::copy(&src, &dst).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to copy {src} to {dst}: {e}"
                ))))
            })?;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("temp_dir", |_args, _out| {
        Ok(VmValue::String(Rc::from(
            std::env::temp_dir().to_string_lossy().to_string().as_str(),
        )))
    });

    vm.register_builtin("stat", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let metadata = std::fs::metadata(&path).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to stat {path}: {e}"
            ))))
        })?;
        let mut info = BTreeMap::new();
        info.insert("size".to_string(), VmValue::Int(metadata.len() as i64));
        info.insert("is_file".to_string(), VmValue::Bool(metadata.is_file()));
        info.insert("is_dir".to_string(), VmValue::Bool(metadata.is_dir()));
        info.insert(
            "readonly".to_string(),
            VmValue::Bool(metadata.permissions().readonly()),
        );
        if let Ok(modified) = metadata.modified() {
            if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                info.insert("modified".to_string(), VmValue::Float(dur.as_secs_f64()));
            }
        }
        Ok(VmValue::Dict(Rc::new(info)))
    });

    // =========================================================================
    // Process execution builtins
    // =========================================================================

    vm.register_builtin("exec", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "exec: command is required",
            ))));
        }
        let cmd = args[0].display();
        let cmd_args: Vec<String> = args[1..].iter().map(|a| a.display()).collect();
        let output = std::process::Command::new(&cmd)
            .args(&cmd_args)
            .output()
            .map_err(|e| VmError::Thrown(VmValue::String(Rc::from(format!("exec failed: {e}")))))?;
        Ok(vm_output_to_value(output))
    });

    vm.register_builtin("shell", |args, _out| {
        let cmd = args.first().map(|a| a.display()).unwrap_or_default();
        if cmd.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "shell: command string is required",
            ))));
        }
        let shell = if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        };
        let flag = if cfg!(target_os = "windows") {
            "/C"
        } else {
            "-c"
        };
        let output = std::process::Command::new(shell)
            .arg(flag)
            .arg(&cmd)
            .output()
            .map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!("shell failed: {e}"))))
            })?;
        Ok(vm_output_to_value(output))
    });

    // =========================================================================
    // Date/time builtins
    // =========================================================================

    vm.register_builtin("date_now", |_args, _out| {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let total_secs = now.as_secs();
        let (y, m, d, hour, minute, second, dow) = vm_civil_from_timestamp(total_secs);
        let mut result = BTreeMap::new();
        result.insert("year".to_string(), VmValue::Int(y));
        result.insert("month".to_string(), VmValue::Int(m));
        result.insert("day".to_string(), VmValue::Int(d));
        result.insert("hour".to_string(), VmValue::Int(hour));
        result.insert("minute".to_string(), VmValue::Int(minute));
        result.insert("second".to_string(), VmValue::Int(second));
        result.insert("weekday".to_string(), VmValue::Int(dow));
        result.insert("timestamp".to_string(), VmValue::Float(now.as_secs_f64()));
        Ok(VmValue::Dict(Rc::new(result)))
    });

    vm.register_builtin("date_format", |args, _out| {
        let ts = match args.first() {
            Some(VmValue::Float(f)) => *f,
            Some(VmValue::Int(n)) => *n as f64,
            Some(VmValue::Dict(map)) => map
                .get("timestamp")
                .and_then(|v| match v {
                    VmValue::Float(f) => Some(*f),
                    VmValue::Int(n) => Some(*n as f64),
                    _ => None,
                })
                .unwrap_or(0.0),
            _ => 0.0,
        };
        let fmt = args
            .get(1)
            .map(|a| a.display())
            .unwrap_or_else(|| "%Y-%m-%d %H:%M:%S".to_string());

        let (y, m, d, hour, minute, second, _dow) = vm_civil_from_timestamp(ts as u64);

        let result = fmt
            .replace("%Y", &format!("{y:04}"))
            .replace("%m", &format!("{m:02}"))
            .replace("%d", &format!("{d:02}"))
            .replace("%H", &format!("{hour:02}"))
            .replace("%M", &format!("{minute:02}"))
            .replace("%S", &format!("{second:02}"));

        Ok(VmValue::String(Rc::from(result.as_str())))
    });

    vm.register_builtin("date_parse", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let parts: Vec<&str> = s.split(|c: char| !c.is_ascii_digit()).collect();
        let parts: Vec<i64> = parts.iter().filter_map(|p| p.parse().ok()).collect();
        if parts.len() < 3 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "Cannot parse date: {s}"
            )))));
        }
        let (y, m, d) = (parts[0], parts[1], parts[2]);
        let hour = parts.get(3).copied().unwrap_or(0);
        let minute = parts.get(4).copied().unwrap_or(0);
        let second = parts.get(5).copied().unwrap_or(0);

        let (y_adj, m_adj) = if m <= 2 {
            (y - 1, (m + 9) as u64)
        } else {
            (y, (m - 3) as u64)
        };
        let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
        let yoe = (y_adj - era * 400) as u64;
        let doy = (153 * m_adj + 2) / 5 + d as u64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146097 + doe as i64 - 719468;
        let ts = days * 86400 + hour * 3600 + minute * 60 + second;
        Ok(VmValue::Float(ts as f64))
    });

    // =========================================================================
    // String formatting
    // =========================================================================

    vm.register_builtin("format", |args, _out| {
        let template = args.first().map(|a| a.display()).unwrap_or_default();
        let mut result = String::with_capacity(template.len());
        let mut arg_iter = args.iter().skip(1);
        let mut rest = template.as_str();
        while let Some(pos) = rest.find("{}") {
            result.push_str(&rest[..pos]);
            if let Some(arg) = arg_iter.next() {
                result.push_str(&arg.display());
            } else {
                result.push_str("{}");
            }
            rest = &rest[pos + 2..];
        }
        result.push_str(rest);
        Ok(VmValue::String(Rc::from(result.as_str())))
    });

    // =========================================================================
    // Standalone string function builtins
    // =========================================================================

    vm.register_builtin("trim", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(s.trim())))
    });

    vm.register_builtin("lowercase", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(s.to_lowercase().as_str())))
    });

    vm.register_builtin("uppercase", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(s.to_uppercase().as_str())))
    });

    vm.register_builtin("split", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let sep = args
            .get(1)
            .map(|a| a.display())
            .unwrap_or_else(|| " ".to_string());
        let parts: Vec<VmValue> = s
            .split(&sep)
            .map(|p| VmValue::String(Rc::from(p)))
            .collect();
        Ok(VmValue::List(Rc::new(parts)))
    });

    vm.register_builtin("starts_with", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let prefix = args.get(1).map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::Bool(s.starts_with(&prefix)))
    });

    vm.register_builtin("ends_with", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let suffix = args.get(1).map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::Bool(s.ends_with(&suffix)))
    });

    vm.register_builtin("contains", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::String(s) => {
                let substr = args.get(1).map(|a| a.display()).unwrap_or_default();
                Ok(VmValue::Bool(s.contains(&substr)))
            }
            VmValue::List(items) => {
                let target = args.get(1).unwrap_or(&VmValue::Nil);
                Ok(VmValue::Bool(
                    items.iter().any(|item| values_equal(item, target)),
                ))
            }
            _ => Ok(VmValue::Bool(false)),
        }
    });

    vm.register_builtin("replace", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let old = args.get(1).map(|a| a.display()).unwrap_or_default();
        let new = args.get(2).map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::String(Rc::from(s.replace(&old, &new).as_str())))
    });

    vm.register_builtin("join", |args, _out| {
        let sep = args.get(1).map(|a| a.display()).unwrap_or_default();
        match args.first() {
            Some(VmValue::List(items)) => {
                let parts: Vec<String> = items.iter().map(|v| v.display()).collect();
                Ok(VmValue::String(Rc::from(parts.join(&sep).as_str())))
            }
            _ => Ok(VmValue::String(Rc::from(""))),
        }
    });

    vm.register_builtin("len", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::String(s) => Ok(VmValue::Int(s.len() as i64)),
            VmValue::List(items) => Ok(VmValue::Int(items.len() as i64)),
            VmValue::Dict(map) => Ok(VmValue::Int(map.len() as i64)),
            _ => Ok(VmValue::Int(0)),
        }
    });

    vm.register_builtin("substring", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let start = args.get(1).and_then(|a| a.as_int()).unwrap_or(0) as usize;
        let start = start.min(s.len());
        match args.get(2).and_then(|a| a.as_int()) {
            Some(length) => {
                let length = (length as usize).min(s.len() - start);
                Ok(VmValue::String(Rc::from(&s[start..start + length])))
            }
            None => Ok(VmValue::String(Rc::from(&s[start..]))),
        }
    });

    // =========================================================================
    // Path builtins
    // =========================================================================

    vm.register_builtin("dirname", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let p = std::path::Path::new(&path);
        match p.parent() {
            Some(parent) => Ok(VmValue::String(Rc::from(parent.to_string_lossy().as_ref()))),
            None => Ok(VmValue::String(Rc::from(""))),
        }
    });

    vm.register_builtin("basename", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let p = std::path::Path::new(&path);
        match p.file_name() {
            Some(name) => Ok(VmValue::String(Rc::from(name.to_string_lossy().as_ref()))),
            None => Ok(VmValue::String(Rc::from(""))),
        }
    });

    vm.register_builtin("extname", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let p = std::path::Path::new(&path);
        match p.extension() {
            Some(ext) => Ok(VmValue::String(Rc::from(
                format!(".{}", ext.to_string_lossy()).as_str(),
            ))),
            None => Ok(VmValue::String(Rc::from(""))),
        }
    });

    // =========================================================================
    // Prompt template rendering
    // =========================================================================

    vm.register_builtin("render", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let template = std::fs::read_to_string(&path).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to read template {path}: {e}"
            ))))
        })?;
        if let Some(bindings) = args.get(1).and_then(|a| a.as_dict()) {
            let mut result = template;
            for (key, val) in bindings.iter() {
                result = result.replace(&format!("{{{{{key}}}}}"), &val.display());
            }
            Ok(VmValue::String(Rc::from(result)))
        } else {
            Ok(VmValue::String(Rc::from(template)))
        }
    });

    // =========================================================================
    // Logging builtins
    // =========================================================================

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
        match vm_level_to_u8(&level_str) {
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

    // =========================================================================
    // Tracing builtins
    // =========================================================================

    vm.register_builtin("trace_start", |args, _out| {
        use rand::Rng;
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        let trace_id = VM_TRACE_STACK.with(|stack| {
            stack
                .borrow()
                .last()
                .map(|t| t.trace_id.clone())
                .unwrap_or_else(|| {
                    let val: u32 = rand::thread_rng().gen();
                    format!("{val:08x}")
                })
        });
        let span_id = {
            let val: u32 = rand::thread_rng().gen();
            format!("{val:08x}")
        };
        let start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        VM_TRACE_STACK.with(|stack| {
            stack.borrow_mut().push(VmTraceContext {
                trace_id: trace_id.clone(),
                span_id: span_id.clone(),
            });
        });

        let mut span = BTreeMap::new();
        span.insert(
            "trace_id".to_string(),
            VmValue::String(Rc::from(trace_id.as_str())),
        );
        span.insert(
            "span_id".to_string(),
            VmValue::String(Rc::from(span_id.as_str())),
        );
        span.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
        span.insert("start_ms".to_string(), VmValue::Int(start_ms));
        Ok(VmValue::Dict(Rc::new(span)))
    });

    vm.register_builtin("trace_end", |args, out| {
        let span = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "trace_end: argument must be a span dict from trace_start",
                ))));
            }
        };

        let end_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let start_ms = span
            .get("start_ms")
            .and_then(|v| v.as_int())
            .unwrap_or(end_ms);
        let duration_ms = end_ms - start_ms;
        let name = span.get("name").map(|v| v.display()).unwrap_or_default();
        let trace_id = span
            .get("trace_id")
            .map(|v| v.display())
            .unwrap_or_default();
        let span_id = span.get("span_id").map(|v| v.display()).unwrap_or_default();

        VM_TRACE_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });

        let level_num = 1_u8;
        if level_num >= VM_MIN_LOG_LEVEL.load(Ordering::Relaxed) {
            let mut fields = BTreeMap::new();
            fields.insert(
                "trace_id".to_string(),
                VmValue::String(Rc::from(trace_id.as_str())),
            );
            fields.insert(
                "span_id".to_string(),
                VmValue::String(Rc::from(span_id.as_str())),
            );
            fields.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
            fields.insert("duration_ms".to_string(), VmValue::Int(duration_ms));
            let line = vm_build_log_line("info", "span_end", Some(&fields));
            out.push_str(&line);
        }

        Ok(VmValue::Nil)
    });

    vm.register_builtin("trace_id", |_args, _out| {
        let id = VM_TRACE_STACK.with(|stack| stack.borrow().last().map(|t| t.trace_id.clone()));
        match id {
            Some(trace_id) => Ok(VmValue::String(Rc::from(trace_id.as_str()))),
            None => Ok(VmValue::Nil),
        }
    });

    // =========================================================================
    // Tool registry builtins
    // =========================================================================

    vm.register_builtin("tool_registry", |_args, _out| {
        let mut registry = BTreeMap::new();
        registry.insert(
            "_type".to_string(),
            VmValue::String(Rc::from("tool_registry")),
        );
        registry.insert("tools".to_string(), VmValue::List(Rc::new(Vec::new())));
        Ok(VmValue::Dict(Rc::new(registry)))
    });

    vm.register_builtin("tool_add", |args, _out| {
        if args.len() < 4 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_add: requires registry, name, description, and handler",
            ))));
        }

        let registry = match &args[0] {
            VmValue::Dict(map) => (**map).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_add: first argument must be a tool registry",
                ))));
            }
        };

        match registry.get("_type") {
            Some(VmValue::String(t)) if &**t == "tool_registry" => {}
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_add: first argument must be a tool registry",
                ))));
            }
        }

        let name = args[1].display();
        let description = args[2].display();
        let handler = args[3].clone();
        let parameters = if args.len() > 4 {
            args[4].clone()
        } else {
            VmValue::Dict(Rc::new(BTreeMap::new()))
        };

        let mut tool_entry = BTreeMap::new();
        tool_entry.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
        tool_entry.insert(
            "description".to_string(),
            VmValue::String(Rc::from(description.as_str())),
        );
        tool_entry.insert("handler".to_string(), handler);
        tool_entry.insert("parameters".to_string(), parameters);

        let mut tools: Vec<VmValue> = match registry.get("tools") {
            Some(VmValue::List(list)) => list
                .iter()
                .filter(|t| {
                    if let VmValue::Dict(e) = t {
                        e.get("name").map(|v| v.display()).as_deref() != Some(name.as_str())
                    } else {
                        true
                    }
                })
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        tools.push(VmValue::Dict(Rc::new(tool_entry)));

        let mut new_registry = registry;
        new_registry.insert("tools".to_string(), VmValue::List(Rc::new(tools)));
        Ok(VmValue::Dict(Rc::new(new_registry)))
    });

    vm.register_builtin("tool_list", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_list: requires a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_list", registry)?;

        let tools = vm_get_tools(registry);
        let mut result = Vec::new();
        for tool in tools {
            if let VmValue::Dict(entry) = tool {
                let mut desc = BTreeMap::new();
                if let Some(name) = entry.get("name") {
                    desc.insert("name".to_string(), name.clone());
                }
                if let Some(description) = entry.get("description") {
                    desc.insert("description".to_string(), description.clone());
                }
                if let Some(parameters) = entry.get("parameters") {
                    desc.insert("parameters".to_string(), parameters.clone());
                }
                result.push(VmValue::Dict(Rc::new(desc)));
            }
        }
        Ok(VmValue::List(Rc::new(result)))
    });

    vm.register_builtin("tool_find", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_find: requires registry and name",
            ))));
        }

        let registry = match &args[0] {
            VmValue::Dict(map) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_find: first argument must be a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_find", registry)?;

        let target_name = args[1].display();
        let tools = vm_get_tools(registry);

        for tool in tools {
            if let VmValue::Dict(entry) = tool {
                if let Some(VmValue::String(name)) = entry.get("name") {
                    if &**name == target_name.as_str() {
                        return Ok(tool.clone());
                    }
                }
            }
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("tool_describe", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_describe: requires a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_describe", registry)?;

        let tools = vm_get_tools(registry);

        if tools.is_empty() {
            return Ok(VmValue::String(Rc::from("Available tools:\n(none)")));
        }

        let mut tool_infos: Vec<(String, String, String)> = Vec::new();
        for tool in tools {
            if let VmValue::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let params_str = vm_format_parameters(entry.get("parameters"));
                tool_infos.push((name, params_str, description));
            }
        }

        tool_infos.sort_by(|a, b| a.0.cmp(&b.0));

        let mut lines = vec!["Available tools:".to_string()];
        for (name, params, desc) in &tool_infos {
            lines.push(format!("- {name}({params}): {desc}"));
        }

        Ok(VmValue::String(Rc::from(lines.join("\n").as_str())))
    });

    vm.register_builtin("tool_remove", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_remove: requires registry and name",
            ))));
        }

        let registry = match &args[0] {
            VmValue::Dict(map) => (**map).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_remove: first argument must be a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_remove", &registry)?;

        let target_name = args[1].display();

        let tools = match registry.get("tools") {
            Some(VmValue::List(list)) => (**list).clone(),
            _ => Vec::new(),
        };

        let filtered: Vec<VmValue> = tools
            .into_iter()
            .filter(|tool| {
                if let VmValue::Dict(entry) = tool {
                    if let Some(VmValue::String(name)) = entry.get("name") {
                        return &**name != target_name.as_str();
                    }
                }
                true
            })
            .collect();

        let mut new_registry = registry;
        new_registry.insert("tools".to_string(), VmValue::List(Rc::new(filtered)));
        Ok(VmValue::Dict(Rc::new(new_registry)))
    });

    vm.register_builtin("tool_count", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_count: requires a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_count", registry)?;
        let count = vm_get_tools(registry).len();
        Ok(VmValue::Int(count as i64))
    });

    vm.register_builtin("tool_schema", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => {
                vm_validate_registry("tool_schema", map)?;
                map
            }
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_schema: requires a tool registry",
                ))));
            }
        };

        let components = args.get(1).and_then(|v| v.as_dict()).cloned();

        let tools = match registry.get("tools") {
            Some(VmValue::List(list)) => list,
            _ => return Ok(VmValue::Dict(Rc::new(vm_build_empty_schema()))),
        };

        let mut tool_schemas = Vec::new();
        for tool in tools.iter() {
            if let VmValue::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();

                let input_schema =
                    vm_build_input_schema(entry.get("parameters"), components.as_ref());

                let mut tool_def = BTreeMap::new();
                tool_def.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
                tool_def.insert(
                    "description".to_string(),
                    VmValue::String(Rc::from(description.as_str())),
                );
                tool_def.insert("inputSchema".to_string(), input_schema);
                tool_schemas.push(VmValue::Dict(Rc::new(tool_def)));
            }
        }

        let mut schema = BTreeMap::new();
        schema.insert(
            "schema_version".to_string(),
            VmValue::String(Rc::from("harn-tools/1.0")),
        );

        if let Some(comps) = &components {
            let mut comp_wrapper = BTreeMap::new();
            comp_wrapper.insert("schemas".to_string(), VmValue::Dict(Rc::new(comps.clone())));
            schema.insert(
                "components".to_string(),
                VmValue::Dict(Rc::new(comp_wrapper)),
            );
        }

        schema.insert("tools".to_string(), VmValue::List(Rc::new(tool_schemas)));
        Ok(VmValue::Dict(Rc::new(schema)))
    });

    vm.register_builtin("tool_parse_call", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();

        let mut results = Vec::new();
        let mut search_from = 0;

        while let Some(start) = text[search_from..].find("<tool_call>") {
            let abs_start = search_from + start + "<tool_call>".len();
            if let Some(end) = text[abs_start..].find("</tool_call>") {
                let json_str = text[abs_start..abs_start + end].trim();
                if let Ok(jv) = serde_json::from_str::<serde_json::Value>(json_str) {
                    results.push(json_to_vm_value(&jv));
                }
                search_from = abs_start + end + "</tool_call>".len();
            } else {
                break;
            }
        }

        Ok(VmValue::List(Rc::new(results)))
    });

    vm.register_builtin("tool_format_result", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_format_result: requires name and result",
            ))));
        }
        let name = args[0].display();
        let result = args[1].display();

        let json_name = vm_escape_json_str(&name);
        let json_result = vm_escape_json_str(&result);
        Ok(VmValue::String(Rc::from(
            format!(
                "<tool_result>{{\"name\": \"{json_name}\", \"result\": \"{json_result}\"}}</tool_result>"
            )
            .as_str(),
        )))
    });

    vm.register_builtin("tool_prompt", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => {
                vm_validate_registry("tool_prompt", map)?;
                map
            }
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_prompt: requires a tool registry",
                ))));
            }
        };

        let tools = match registry.get("tools") {
            Some(VmValue::List(list)) => list,
            _ => {
                return Ok(VmValue::String(Rc::from("No tools are available.")));
            }
        };

        if tools.is_empty() {
            return Ok(VmValue::String(Rc::from("No tools are available.")));
        }

        let mut prompt = String::from("# Available Tools\n\n");
        prompt.push_str("You have access to the following tools. To use a tool, output a tool call in this exact format:\n\n");
        prompt.push_str("<tool_call>{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}</tool_call>\n\n");
        prompt.push_str("You may make multiple tool calls in a single response. Wait for tool results before proceeding.\n\n");
        prompt.push_str("## Tools\n\n");

        let mut tool_infos: Vec<(&BTreeMap<String, VmValue>, String)> = Vec::new();
        for tool in tools.iter() {
            if let VmValue::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                tool_infos.push((entry, name));
            }
        }
        tool_infos.sort_by(|a, b| a.1.cmp(&b.1));

        for (entry, name) in &tool_infos {
            let description = entry
                .get("description")
                .map(|v| v.display())
                .unwrap_or_default();
            let params_str = vm_format_parameters(entry.get("parameters"));

            prompt.push_str(&format!("### {name}\n"));
            prompt.push_str(&format!("{description}\n"));
            if !params_str.is_empty() {
                prompt.push_str(&format!("Parameters: {params_str}\n"));
            }
            prompt.push('\n');
        }

        Ok(VmValue::String(Rc::from(prompt.trim_end())))
    });

    // =========================================================================
    // Channel builtins (sync)
    // =========================================================================

    vm.register_builtin("channel", |args, _out| {
        let name = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| "default".to_string());
        let capacity = args.get(1).and_then(|a| a.as_int()).unwrap_or(256) as usize;
        let capacity = capacity.max(1);
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        #[allow(clippy::arc_with_non_send_sync)]
        Ok(VmValue::Channel(VmChannelHandle {
            name,
            sender: Arc::new(tx),
            receiver: Arc::new(tokio::sync::Mutex::new(rx)),
            closed: Arc::new(AtomicBool::new(false)),
        }))
    });

    vm.register_builtin("close_channel", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "close_channel: requires a channel",
            ))));
        }
        if let VmValue::Channel(ch) = &args[0] {
            ch.closed.store(true, Ordering::SeqCst);
            Ok(VmValue::Nil)
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "close_channel: first argument must be a channel",
            ))))
        }
    });

    vm.register_builtin("try_receive", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "try_receive: requires a channel",
            ))));
        }
        if let VmValue::Channel(ch) = &args[0] {
            match ch.receiver.try_lock() {
                Ok(mut rx) => match rx.try_recv() {
                    Ok(val) => Ok(val),
                    Err(_) => Ok(VmValue::Nil),
                },
                Err(_) => Ok(VmValue::Nil),
            }
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "try_receive: first argument must be a channel",
            ))))
        }
    });

    // =========================================================================
    // Atomic builtins
    // =========================================================================

    vm.register_builtin("atomic", |args, _out| {
        let initial = match args.first() {
            Some(VmValue::Int(n)) => *n,
            Some(VmValue::Float(f)) => *f as i64,
            Some(VmValue::Bool(b)) => {
                if *b {
                    1
                } else {
                    0
                }
            }
            _ => 0,
        };
        Ok(VmValue::Atomic(VmAtomicHandle {
            value: Arc::new(AtomicI64::new(initial)),
        }))
    });

    vm.register_builtin("atomic_get", |args, _out| {
        if let Some(VmValue::Atomic(a)) = args.first() {
            Ok(VmValue::Int(a.value.load(Ordering::SeqCst)))
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("atomic_set", |args, _out| {
        if args.len() >= 2 {
            if let (VmValue::Atomic(a), Some(val)) = (&args[0], args[1].as_int()) {
                let old = a.value.swap(val, Ordering::SeqCst);
                return Ok(VmValue::Int(old));
            }
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("atomic_add", |args, _out| {
        if args.len() >= 2 {
            if let (VmValue::Atomic(a), Some(delta)) = (&args[0], args[1].as_int()) {
                let prev = a.value.fetch_add(delta, Ordering::SeqCst);
                return Ok(VmValue::Int(prev));
            }
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("atomic_cas", |args, _out| {
        if args.len() >= 3 {
            if let (VmValue::Atomic(a), Some(expected), Some(new_val)) =
                (&args[0], args[1].as_int(), args[2].as_int())
            {
                let result =
                    a.value
                        .compare_exchange(expected, new_val, Ordering::SeqCst, Ordering::SeqCst);
                return Ok(VmValue::Bool(result.is_ok()));
            }
        }
        Ok(VmValue::Bool(false))
    });

    // =========================================================================
    // Async builtins
    // =========================================================================

    // sleep(ms)
    vm.register_async_builtin("sleep", |args| async move {
        let ms = match args.first() {
            Some(VmValue::Duration(ms)) => *ms,
            Some(VmValue::Int(n)) => *n as u64,
            _ => 0,
        };
        if ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
        }
        Ok(VmValue::Nil)
    });

    // send(channel, value)
    vm.register_async_builtin("send", |args| async move {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "send: requires channel and value",
            ))));
        }
        if let VmValue::Channel(ch) = &args[0] {
            if ch.closed.load(Ordering::SeqCst) {
                return Ok(VmValue::Bool(false));
            }
            let val = args[1].clone();
            match ch.sender.send(val).await {
                Ok(()) => Ok(VmValue::Bool(true)),
                Err(_) => Ok(VmValue::Bool(false)),
            }
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "send: first argument must be a channel",
            ))))
        }
    });

    // receive(channel)
    vm.register_async_builtin("receive", |args| async move {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "receive: requires a channel",
            ))));
        }
        if let VmValue::Channel(ch) = &args[0] {
            if ch.closed.load(Ordering::SeqCst) {
                let mut rx = ch.receiver.lock().await;
                return match rx.try_recv() {
                    Ok(val) => Ok(val),
                    Err(_) => Ok(VmValue::Nil),
                };
            }
            let mut rx = ch.receiver.lock().await;
            match rx.recv().await {
                Some(val) => Ok(val),
                None => Ok(VmValue::Nil),
            }
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "receive: first argument must be a channel",
            ))))
        }
    });

    // select(channel1, channel2, ...) — blocking multiplexed receive (variadic)
    vm.register_async_builtin("select", |args| async move {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "select: requires at least one channel",
            ))));
        }
        for arg in &args {
            if !matches!(arg, VmValue::Channel(_)) {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "select: all arguments must be channels",
                ))));
            }
        }
        loop {
            let (found, all_closed) = try_poll_channels(&args);
            if let Some((i, val, name)) = found {
                return Ok(select_result(i, val, &name));
            }
            if all_closed {
                return Ok(select_none());
            }
            tokio::task::yield_now().await;
        }
    });

    // __select_timeout(channel_list, timeout_ms) — select with timeout
    vm.register_async_builtin("__select_timeout", |args| async move {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "__select_timeout: requires channel list and timeout",
            ))));
        }
        let channels = match &args[0] {
            VmValue::List(items) => (**items).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "__select_timeout: first argument must be a list of channels",
                ))));
            }
        };
        let timeout_ms = match &args[1] {
            VmValue::Int(n) => (*n).max(0) as u64,
            VmValue::Duration(ms) => *ms,
            _ => 5000,
        };
        let deadline = tokio::time::Instant::now()
            + tokio::time::Duration::from_millis(timeout_ms);
        loop {
            let (found, all_closed) = try_poll_channels(&channels);
            if let Some((i, val, name)) = found {
                return Ok(select_result(i, val, &name));
            }
            if all_closed || tokio::time::Instant::now() >= deadline {
                return Ok(select_none());
            }
            tokio::task::yield_now().await;
        }
    });

    // __select_try(channel_list) — non-blocking select (for default case)
    vm.register_async_builtin("__select_try", |args| async move {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "__select_try: requires channel list",
            ))));
        }
        let channels = match &args[0] {
            VmValue::List(items) => (**items).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "__select_try: first argument must be a list of channels",
                ))));
            }
        };
        let (found, _) = try_poll_channels(&channels);
        if let Some((i, val, name)) = found {
            Ok(select_result(i, val, &name))
        } else {
            Ok(select_none())
        }
    });

    // __select_list(channel_list) — blocking select from a list of channels
    vm.register_async_builtin("__select_list", |args| async move {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "__select_list: requires channel list",
            ))));
        }
        let channels = match &args[0] {
            VmValue::List(items) => (**items).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "__select_list: first argument must be a list of channels",
                ))));
            }
        };
        loop {
            let (found, all_closed) = try_poll_channels(&channels);
            if let Some((i, val, name)) = found {
                return Ok(select_result(i, val, &name));
            }
            if all_closed {
                return Ok(select_none());
            }
            tokio::task::yield_now().await;
        }
    });

    // =========================================================================
    // JSON validation and extraction builtins
    // =========================================================================

    vm.register_builtin("json_validate", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "json_validate requires 2 arguments: data and schema",
            ))));
        }
        let data = &args[0];
        let schema = &args[1];
        let schema_dict = match schema.as_dict() {
            Some(d) => d,
            None => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "json_validate: schema must be a dict",
                ))));
            }
        };
        let mut errors = Vec::new();
        validate_value(data, schema_dict, "", &mut errors);
        if errors.is_empty() {
            Ok(VmValue::Bool(true))
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                errors.join("; "),
            ))))
        }
    });

    vm.register_builtin("json_extract", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "json_extract requires at least 1 argument: text",
            ))));
        }
        let text = args[0].display();
        let key = args.get(1).map(|a| a.display());

        // Extract JSON from text that may contain markdown code fences
        let json_str = extract_json_from_text(&text);
        let parsed = match serde_json::from_str::<serde_json::Value>(&json_str) {
            Ok(jv) => json_to_vm_value(&jv),
            Err(e) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "json_extract: failed to parse JSON: {e}"
                )))));
            }
        };

        match key {
            Some(k) => match &parsed {
                VmValue::Dict(map) => match map.get(&k) {
                    Some(val) => Ok(val.clone()),
                    None => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "json_extract: key '{}' not found",
                        k
                    ))))),
                },
                _ => Err(VmError::Thrown(VmValue::String(Rc::from(
                    "json_extract: parsed value is not a dict, cannot extract key",
                )))),
            },
            None => Ok(parsed),
        }
    });

    // =========================================================================
    // HTTP and LLM builtins (registered from separate modules)
    // =========================================================================

    // =========================================================================
    // Internal builtins (used by compiler-generated code)
    // =========================================================================

    vm.register_builtin("__assert_dict", |args, _out| {
        let val = args.first().cloned().unwrap_or(VmValue::Nil);
        if matches!(val, VmValue::Dict(_)) {
            Ok(VmValue::Nil)
        } else {
            Err(VmError::TypeError(format!(
                "cannot destructure {} with {{...}} pattern — expected dict",
                val.type_name()
            )))
        }
    });

    vm.register_builtin("__assert_list", |args, _out| {
        let val = args.first().cloned().unwrap_or(VmValue::Nil);
        if matches!(val, VmValue::List(_)) {
            Ok(VmValue::Nil)
        } else {
            Err(VmError::TypeError(format!(
                "cannot destructure {} with [...] pattern — expected list",
                val.type_name()
            )))
        }
    });

    vm.register_builtin("__dict_rest", |args, _out| {
        // __dict_rest(dict, keys_to_exclude) -> new dict without those keys
        let dict = args.first().cloned().unwrap_or(VmValue::Nil);
        let keys_list = args.get(1).cloned().unwrap_or(VmValue::Nil);
        if let VmValue::Dict(map) = dict {
            let exclude: std::collections::HashSet<String> = match keys_list {
                VmValue::List(items) => items
                    .iter()
                    .filter_map(|v| {
                        if let VmValue::String(s) = v {
                            Some(s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect(),
                _ => std::collections::HashSet::new(),
            };
            let rest: BTreeMap<String, VmValue> = map
                .iter()
                .filter(|(k, _)| !exclude.contains(k.as_str()))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Ok(VmValue::Dict(Rc::new(rest)))
        } else {
            Ok(VmValue::Nil)
        }
    });

    register_http_builtins(vm);
    register_llm_builtins(vm);
    register_mcp_builtins(vm);
}

// =============================================================================
// JSON helpers
// =============================================================================

pub(crate) fn escape_json_string_vm(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub(crate) fn vm_value_to_json(val: &VmValue) -> String {
    match val {
        VmValue::String(s) => escape_json_string_vm(s),
        VmValue::Int(n) => n.to_string(),
        VmValue::Float(n) => n.to_string(),
        VmValue::Bool(b) => b.to_string(),
        VmValue::Nil => "null".to_string(),
        VmValue::List(items) => {
            let inner: Vec<String> = items.iter().map(vm_value_to_json).collect();
            format!("[{}]", inner.join(","))
        }
        VmValue::Dict(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}:{}", escape_json_string_vm(k), vm_value_to_json(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        _ => "null".to_string(),
    }
}

pub(crate) fn json_to_vm_value(jv: &serde_json::Value) -> VmValue {
    match jv {
        serde_json::Value::Null => VmValue::Nil,
        serde_json::Value::Bool(b) => VmValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                VmValue::Int(i)
            } else {
                VmValue::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => VmValue::String(Rc::from(s.as_str())),
        serde_json::Value::Array(arr) => {
            VmValue::List(Rc::new(arr.iter().map(json_to_vm_value).collect()))
        }
        serde_json::Value::Object(map) => {
            let mut m = BTreeMap::new();
            for (k, v) in map {
                m.insert(k.clone(), json_to_vm_value(v));
            }
            VmValue::Dict(Rc::new(m))
        }
    }
}

// =============================================================================
// Helper: validate a VmValue against a schema dict
// =============================================================================

fn validate_value(
    value: &VmValue,
    schema: &BTreeMap<String, VmValue>,
    path: &str,
    errors: &mut Vec<String>,
) {
    // Check "type" constraint
    if let Some(VmValue::String(expected_type)) = schema.get("type") {
        let actual_type = value.type_name();
        let type_str: &str = expected_type;
        if type_str != "any" && actual_type != type_str {
            let location = if path.is_empty() {
                "root".to_string()
            } else {
                path.to_string()
            };
            errors.push(format!(
                "at {}: expected type '{}', got '{}'",
                location, type_str, actual_type
            ));
            return; // No point checking further if type is wrong
        }
    }

    // Check "required" keys (only for dicts)
    if let Some(VmValue::List(required_keys)) = schema.get("required") {
        if let VmValue::Dict(map) = value {
            for key_val in required_keys.iter() {
                let key = key_val.display();
                if !map.contains_key(&key) {
                    let location = if path.is_empty() {
                        "root".to_string()
                    } else {
                        path.to_string()
                    };
                    errors.push(format!("at {}: missing required key '{}'", location, key));
                }
            }
        }
    }

    // Check "properties" (only for dicts)
    if let Some(VmValue::Dict(prop_schemas)) = schema.get("properties") {
        if let VmValue::Dict(map) = value {
            for (key, prop_schema) in prop_schemas.iter() {
                if let Some(prop_value) = map.get(key) {
                    if let Some(prop_schema_dict) = prop_schema.as_dict() {
                        let child_path = if path.is_empty() {
                            key.clone()
                        } else {
                            format!("{}.{}", path, key)
                        };
                        validate_value(prop_value, prop_schema_dict, &child_path, errors);
                    }
                }
            }
        }
    }

    // Check "items" (only for lists)
    if let Some(VmValue::Dict(item_schema)) = schema.get("items") {
        if let VmValue::List(items) = value {
            for (i, item) in items.iter().enumerate() {
                let child_path = if path.is_empty() {
                    format!("[{}]", i)
                } else {
                    format!("{}[{}]", path, i)
                };
                validate_value(item, item_schema, &child_path, errors);
            }
        }
    }
}

// =============================================================================
// Helper: extract JSON from text with possible markdown fences
// =============================================================================

fn extract_json_from_text(text: &str) -> String {
    let trimmed = text.trim();

    // Try to find ```json ... ``` or ``` ... ``` code fences
    if let Some(start) = trimmed.find("```") {
        let after_backticks = &trimmed[start + 3..];
        // Skip optional language tag (e.g., "json")
        let content_start = if let Some(nl) = after_backticks.find('\n') {
            nl + 1
        } else {
            0
        };
        let content = &after_backticks[content_start..];
        if let Some(end) = content.find("```") {
            return content[..end].trim().to_string();
        }
    }

    // No code fences found; try to find JSON object or array boundaries
    // Look for first { or [ and last matching } or ]
    if let Some(obj_start) = trimmed.find('{') {
        if let Some(obj_end) = trimmed.rfind('}') {
            if obj_end > obj_start {
                return trimmed[obj_start..=obj_end].to_string();
            }
        }
    }
    if let Some(arr_start) = trimmed.find('[') {
        if let Some(arr_end) = trimmed.rfind(']') {
            if arr_end > arr_start {
                return trimmed[arr_start..=arr_end].to_string();
            }
        }
    }

    // Fall back to trimmed text as-is
    trimmed.to_string()
}

// =============================================================================
// Helper: convert process::Output to VmValue dict
// =============================================================================

fn vm_output_to_value(output: std::process::Output) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert(
        "stdout".to_string(),
        VmValue::String(Rc::from(
            String::from_utf8_lossy(&output.stdout).to_string().as_str(),
        )),
    );
    result.insert(
        "stderr".to_string(),
        VmValue::String(Rc::from(
            String::from_utf8_lossy(&output.stderr).to_string().as_str(),
        )),
    );
    result.insert(
        "status".to_string(),
        VmValue::Int(output.status.code().unwrap_or(-1) as i64),
    );
    result.insert(
        "success".to_string(),
        VmValue::Bool(output.status.success()),
    );
    VmValue::Dict(Rc::new(result))
}

// =============================================================================
// Helper: civil date from timestamp (Howard Hinnant's algorithm)
// =============================================================================

fn vm_civil_from_timestamp(total_secs: u64) -> (i64, i64, i64, i64, i64, i64, i64) {
    let days = total_secs / 86400;
    let time_of_day = total_secs % 86400;
    let hour = (time_of_day / 3600) as i64;
    let minute = ((time_of_day % 3600) / 60) as i64;
    let second = (time_of_day % 60) as i64;

    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as i64;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as i64;
    let y = if m <= 2 { y + 1 } else { y };
    let dow = ((days + 4) % 7) as i64;

    (y, m, d, hour, minute, second, dow)
}

// =============================================================================
// Logging helpers for VM
// =============================================================================

pub(crate) static VM_MIN_LOG_LEVEL: AtomicU8 = AtomicU8::new(0);

#[derive(Clone)]
pub(crate) struct VmTraceContext {
    pub(crate) trace_id: String,
    pub(crate) span_id: String,
}

thread_local! {
    pub(crate) static VM_TRACE_STACK: std::cell::RefCell<Vec<VmTraceContext>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn vm_level_to_u8(level: &str) -> Option<u8> {
    match level {
        "debug" => Some(0),
        "info" => Some(1),
        "warn" => Some(2),
        "error" => Some(3),
        _ => None,
    }
}

fn vm_format_timestamp_utc() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = now.as_secs();
    let millis = now.subsec_millis();

    let days = total_secs / 86400;
    let time_of_day = total_secs % 86400;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

pub(crate) fn vm_escape_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn vm_escape_json_str_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&vm_escape_json_str(s));
    out.push('"');
    out
}

fn vm_value_to_json_fragment(val: &VmValue) -> String {
    match val {
        VmValue::String(s) => vm_escape_json_str_quoted(s),
        VmValue::Int(n) => n.to_string(),
        VmValue::Float(n) => {
            if n.is_finite() {
                n.to_string()
            } else {
                "null".to_string()
            }
        }
        VmValue::Bool(b) => b.to_string(),
        VmValue::Nil => "null".to_string(),
        _ => vm_escape_json_str_quoted(&val.display()),
    }
}

fn vm_build_log_line(level: &str, msg: &str, fields: Option<&BTreeMap<String, VmValue>>) -> String {
    let ts = vm_format_timestamp_utc();
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("\"ts\":{}", vm_escape_json_str_quoted(&ts)));
    parts.push(format!("\"level\":{}", vm_escape_json_str_quoted(level)));
    parts.push(format!("\"msg\":{}", vm_escape_json_str_quoted(msg)));

    VM_TRACE_STACK.with(|stack| {
        if let Some(trace) = stack.borrow().last() {
            parts.push(format!(
                "\"trace_id\":{}",
                vm_escape_json_str_quoted(&trace.trace_id)
            ));
            parts.push(format!(
                "\"span_id\":{}",
                vm_escape_json_str_quoted(&trace.span_id)
            ));
        }
    });

    if let Some(dict) = fields {
        for (k, v) in dict {
            parts.push(format!(
                "{}:{}",
                vm_escape_json_str_quoted(k),
                vm_value_to_json_fragment(v)
            ));
        }
    }

    format!("{{{}}}\n", parts.join(","))
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

// =============================================================================
// Tool registry helpers for VM
// =============================================================================

fn vm_validate_registry(name: &str, dict: &BTreeMap<String, VmValue>) -> Result<(), VmError> {
    match dict.get("_type") {
        Some(VmValue::String(t)) if &**t == "tool_registry" => Ok(()),
        _ => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{name}: argument must be a tool registry (created with tool_registry())"
        ))))),
    }
}

fn vm_get_tools(dict: &BTreeMap<String, VmValue>) -> &[VmValue] {
    match dict.get("tools") {
        Some(VmValue::List(list)) => list,
        _ => &[],
    }
}

fn vm_format_parameters(params: Option<&VmValue>) -> String {
    match params {
        Some(VmValue::Dict(map)) if !map.is_empty() => {
            let mut pairs: Vec<(String, String)> =
                map.iter().map(|(k, v)| (k.clone(), v.display())).collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            pairs
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
        _ => String::new(),
    }
}

fn vm_build_empty_schema() -> BTreeMap<String, VmValue> {
    let mut schema = BTreeMap::new();
    schema.insert(
        "schema_version".to_string(),
        VmValue::String(Rc::from("harn-tools/1.0")),
    );
    schema.insert("tools".to_string(), VmValue::List(Rc::new(Vec::new())));
    schema
}

fn vm_build_input_schema(
    params: Option<&VmValue>,
    components: Option<&BTreeMap<String, VmValue>>,
) -> VmValue {
    let mut schema = BTreeMap::new();
    schema.insert("type".to_string(), VmValue::String(Rc::from("object")));

    let params_map = match params {
        Some(VmValue::Dict(map)) if !map.is_empty() => map,
        _ => {
            schema.insert(
                "properties".to_string(),
                VmValue::Dict(Rc::new(BTreeMap::new())),
            );
            return VmValue::Dict(Rc::new(schema));
        }
    };

    let mut properties = BTreeMap::new();
    let mut required = Vec::new();

    for (key, val) in params_map.iter() {
        let prop = vm_resolve_param_type(val, components);
        properties.insert(key.clone(), prop);
        required.push(VmValue::String(Rc::from(key.as_str())));
    }

    schema.insert("properties".to_string(), VmValue::Dict(Rc::new(properties)));
    if !required.is_empty() {
        required.sort_by_key(|a| a.display());
        schema.insert("required".to_string(), VmValue::List(Rc::new(required)));
    }

    VmValue::Dict(Rc::new(schema))
}

fn vm_resolve_param_type(val: &VmValue, components: Option<&BTreeMap<String, VmValue>>) -> VmValue {
    match val {
        VmValue::String(type_name) => {
            let json_type = vm_harn_type_to_json_schema(type_name);
            let mut prop = BTreeMap::new();
            prop.insert("type".to_string(), VmValue::String(Rc::from(json_type)));
            VmValue::Dict(Rc::new(prop))
        }
        VmValue::Dict(map) => {
            if let Some(VmValue::String(ref_name)) = map.get("$ref") {
                if let Some(comps) = components {
                    if let Some(resolved) = comps.get(&**ref_name) {
                        return resolved.clone();
                    }
                }
                let mut prop = BTreeMap::new();
                prop.insert(
                    "$ref".to_string(),
                    VmValue::String(Rc::from(
                        format!("#/components/schemas/{ref_name}").as_str(),
                    )),
                );
                VmValue::Dict(Rc::new(prop))
            } else {
                VmValue::Dict(Rc::new((**map).clone()))
            }
        }
        _ => {
            let mut prop = BTreeMap::new();
            prop.insert("type".to_string(), VmValue::String(Rc::from("string")));
            VmValue::Dict(Rc::new(prop))
        }
    }
}

fn vm_harn_type_to_json_schema(harn_type: &str) -> &str {
    match harn_type {
        "int" => "integer",
        "float" => "number",
        "bool" | "boolean" => "boolean",
        "list" | "array" => "array",
        "dict" | "object" => "object",
        _ => "string",
    }
}
