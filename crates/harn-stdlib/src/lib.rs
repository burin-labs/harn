#![allow(clippy::result_large_err)]

mod async_builtins;
mod json;
mod llm;

use harn_runtime::{Interpreter, RuntimeError, Value};

pub use async_builtins::register_async_builtins;
pub use llm::register_llm_builtins;

/// Convert a process::Output into a Harn Dict value.
fn output_to_value(output: std::process::Output) -> Value {
    let mut result = std::collections::BTreeMap::new();
    result.insert(
        "stdout".to_string(),
        Value::String(String::from_utf8_lossy(&output.stdout).to_string()),
    );
    result.insert(
        "stderr".to_string(),
        Value::String(String::from_utf8_lossy(&output.stderr).to_string()),
    );
    result.insert(
        "status".to_string(),
        Value::Int(output.status.code().unwrap_or(-1) as i64),
    );
    result.insert("success".to_string(), Value::Bool(output.status.success()));
    Value::Dict(result)
}

/// Register all standard library builtins on an interpreter.
pub fn register_stdlib(interp: &mut Interpreter) {
    interp.register_builtin("log", |args, out| {
        let msg = args.first().map(|a| a.as_string()).unwrap_or_default();
        out.extend_from_slice(format!("[harn] {msg}\n").as_bytes());
        Ok(Value::Nil)
    });

    interp.register_builtin("print", |args, out| {
        let msg = args.first().map(|a| a.as_string()).unwrap_or_default();
        out.extend_from_slice(msg.as_bytes());
        Ok(Value::Nil)
    });

    interp.register_builtin("println", |args, out| {
        let msg = args.first().map(|a| a.as_string()).unwrap_or_default();
        out.extend_from_slice(format!("{msg}\n").as_bytes());
        Ok(Value::Nil)
    });

    interp.register_builtin("type_of", |args, _out| {
        let val = args.first().unwrap_or(&Value::Nil);
        let type_name = match val {
            Value::String(_) => "string",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Bool(_) => "bool",
            Value::Nil => "nil",
            Value::List(_) => "list",
            Value::Dict(_) => "dict",
            Value::Closure { .. } => "closure",
            Value::TaskHandle { .. } => "taskHandle",
            Value::Duration(_) => "duration",
            Value::EnumVariant { .. } => "enum",
            Value::StructInstance { .. } => "struct",
            Value::Channel(_) => "channel",
            Value::Atomic(_) => "atomic",
        };
        Ok(Value::String(type_name.to_string()))
    });

    interp.register_builtin("to_string", |args, _out| {
        let val = args.first().unwrap_or(&Value::Nil);
        Ok(Value::String(val.as_string()))
    });

    interp.register_builtin("to_int", |args, _out| {
        let val = args.first().unwrap_or(&Value::Nil);
        match val {
            Value::Int(n) => Ok(Value::Int(*n)),
            Value::Float(n) => {
                if n.is_finite() && *n >= i64::MIN as f64 && *n < (i64::MAX as f64) + 1.0 {
                    Ok(Value::Int(*n as i64))
                } else {
                    Ok(Value::Nil)
                }
            }
            Value::String(s) => Ok(s.parse::<i64>().map(Value::Int).unwrap_or(Value::Nil)),
            Value::Bool(b) => Ok(Value::Int(if *b { 1 } else { 0 })),
            _ => Ok(Value::Nil),
        }
    });

    interp.register_builtin("to_float", |args, _out| {
        let val = args.first().unwrap_or(&Value::Nil);
        match val {
            Value::Float(n) => Ok(Value::Float(*n)),
            Value::Int(n) => Ok(Value::Float(*n as f64)),
            Value::String(s) => Ok(s.parse::<f64>().map(Value::Float).unwrap_or(Value::Nil)),
            _ => Ok(Value::Nil),
        }
    });

    interp.register_builtin("json_stringify", |args, _out| {
        let val = args.first().unwrap_or(&Value::Nil);
        Ok(Value::String(value_to_json(val)))
    });

    interp.register_builtin("json_parse", |args, _out| {
        let text = args.first().map(|a| a.as_string()).unwrap_or_default();
        json::json_parse(&text).map_err(|e| RuntimeError::thrown(format!("JSON parse error: {e}")))
    });

    interp.register_builtin("env", |args, _out| {
        let name = args.first().map(|a| a.as_string()).unwrap_or_default();
        match std::env::var(&name) {
            Ok(val) => Ok(Value::String(val)),
            Err(_) => Ok(Value::Nil),
        }
    });

    interp.register_builtin("timestamp", |_args, _out| {
        use std::time::{SystemTime, UNIX_EPOCH};
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        Ok(Value::Float(secs))
    });

    // sleep is registered as an async builtin in register_async_builtins()

    interp.register_builtin("read_file", |args, _out| {
        let path = args.first().map(|a| a.as_string()).unwrap_or_default();
        match std::fs::read_to_string(&path) {
            Ok(content) => Ok(Value::String(content)),
            Err(e) => Err(RuntimeError::thrown(format!(
                "Failed to read file {path}: {e}"
            ))),
        }
    });

    interp.register_builtin("write_file", |args, _out| {
        if args.len() >= 2 {
            let path = args[0].as_string();
            let content = args[1].as_string();
            std::fs::write(&path, &content)
                .map_err(|e| RuntimeError::thrown(format!("Failed to write file {path}: {e}")))?;
        }
        Ok(Value::Nil)
    });

    interp.register_builtin("exit", |args, _out| {
        let code = args.first().and_then(|a| a.as_int()).unwrap_or(0);
        std::process::exit(code as i32);
    });

    interp.register_builtin("regex_match", |args, _out| {
        if args.len() >= 2 {
            let pattern = args[0].as_string();
            let text = args[1].as_string();
            let re = regex::Regex::new(&pattern)
                .map_err(|e| RuntimeError::thrown(format!("Invalid regex: {e}")))?;
            let matches: Vec<Value> = re
                .find_iter(&text)
                .map(|m| Value::String(m.as_str().to_string()))
                .collect();
            if matches.is_empty() {
                return Ok(Value::Nil);
            }
            return Ok(Value::List(matches));
        }
        Ok(Value::Nil)
    });

    // --- Math builtins ---

    interp.register_builtin("abs", |args, _out| {
        match args.first().unwrap_or(&Value::Nil) {
            Value::Int(n) => Ok(Value::Int(n.wrapping_abs())),
            Value::Float(n) => Ok(Value::Float(n.abs())),
            _ => Ok(Value::Nil),
        }
    });

    interp.register_builtin("min", |args, _out| {
        if args.len() >= 2 {
            let a = &args[0];
            let b = &args[1];
            match (a, b) {
                (Value::Int(x), Value::Int(y)) => Ok(Value::Int(*x.min(y))),
                (Value::Float(x), Value::Float(y)) => Ok(Value::Float(x.min(*y))),
                (Value::Int(x), Value::Float(y)) => Ok(Value::Float((*x as f64).min(*y))),
                (Value::Float(x), Value::Int(y)) => Ok(Value::Float(x.min(*y as f64))),
                _ => Ok(Value::Nil),
            }
        } else {
            Ok(Value::Nil)
        }
    });

    interp.register_builtin("max", |args, _out| {
        if args.len() >= 2 {
            let a = &args[0];
            let b = &args[1];
            match (a, b) {
                (Value::Int(x), Value::Int(y)) => Ok(Value::Int(*x.max(y))),
                (Value::Float(x), Value::Float(y)) => Ok(Value::Float(x.max(*y))),
                (Value::Int(x), Value::Float(y)) => Ok(Value::Float((*x as f64).max(*y))),
                (Value::Float(x), Value::Int(y)) => Ok(Value::Float(x.max(*y as f64))),
                _ => Ok(Value::Nil),
            }
        } else {
            Ok(Value::Nil)
        }
    });

    interp.register_builtin("floor", |args, _out| {
        match args.first().unwrap_or(&Value::Nil) {
            Value::Float(n) => {
                let r = n.floor();
                if r.is_finite() && r >= i64::MIN as f64 && r < (i64::MAX as f64) + 1.0 {
                    Ok(Value::Int(r as i64))
                } else {
                    Ok(Value::Nil)
                }
            }
            Value::Int(n) => Ok(Value::Int(*n)),
            _ => Ok(Value::Nil),
        }
    });

    interp.register_builtin("ceil", |args, _out| {
        match args.first().unwrap_or(&Value::Nil) {
            Value::Float(n) => {
                let r = n.ceil();
                if r.is_finite() && r >= i64::MIN as f64 && r < (i64::MAX as f64) + 1.0 {
                    Ok(Value::Int(r as i64))
                } else {
                    Ok(Value::Nil)
                }
            }
            Value::Int(n) => Ok(Value::Int(*n)),
            _ => Ok(Value::Nil),
        }
    });

    interp.register_builtin("round", |args, _out| {
        match args.first().unwrap_or(&Value::Nil) {
            Value::Float(n) => {
                let r = n.round();
                if r.is_finite() && r >= i64::MIN as f64 && r < (i64::MAX as f64) + 1.0 {
                    Ok(Value::Int(r as i64))
                } else {
                    Ok(Value::Nil)
                }
            }
            Value::Int(n) => Ok(Value::Int(*n)),
            _ => Ok(Value::Nil),
        }
    });

    interp.register_builtin("sqrt", |args, _out| {
        match args.first().unwrap_or(&Value::Nil) {
            Value::Float(n) => Ok(Value::Float(n.sqrt())),
            Value::Int(n) => Ok(Value::Float((*n as f64).sqrt())),
            _ => Ok(Value::Nil),
        }
    });

    interp.register_builtin("pow", |args, _out| {
        if args.len() >= 2 {
            match (&args[0], &args[1]) {
                (Value::Int(base), Value::Int(exp)) => {
                    if *exp >= 0 && *exp <= u32::MAX as i64 {
                        Ok(Value::Int(base.wrapping_pow(*exp as u32)))
                    } else {
                        Ok(Value::Float((*base as f64).powf(*exp as f64)))
                    }
                }
                (Value::Float(base), Value::Int(exp)) => {
                    if *exp >= i32::MIN as i64 && *exp <= i32::MAX as i64 {
                        Ok(Value::Float(base.powi(*exp as i32)))
                    } else {
                        Ok(Value::Float(base.powf(*exp as f64)))
                    }
                }
                (Value::Int(base), Value::Float(exp)) => {
                    Ok(Value::Float((*base as f64).powf(*exp)))
                }
                (Value::Float(base), Value::Float(exp)) => Ok(Value::Float(base.powf(*exp))),
                _ => Ok(Value::Nil),
            }
        } else {
            Ok(Value::Nil)
        }
    });

    interp.register_builtin("random", |_args, _out| {
        use rand::Rng;
        let val: f64 = rand::thread_rng().gen();
        Ok(Value::Float(val))
    });

    interp.register_builtin("random_int", |args, _out| {
        use rand::Rng;
        if args.len() >= 2 {
            let min = args[0].as_int().unwrap_or(0);
            let max = args[1].as_int().unwrap_or(0);
            if min <= max {
                let val = rand::thread_rng().gen_range(min..=max);
                return Ok(Value::Int(val));
            }
        }
        Ok(Value::Nil)
    });

    // --- Assert builtins ---

    interp.register_builtin("assert", |args, _out| {
        let condition = args.first().unwrap_or(&Value::Nil);
        if !condition.is_truthy() {
            let msg = args
                .get(1)
                .map(|a| a.as_string())
                .unwrap_or_else(|| "Assertion failed".to_string());
            return Err(RuntimeError::thrown(msg));
        }
        Ok(Value::Nil)
    });

    interp.register_builtin("assert_eq", |args, _out| {
        if args.len() >= 2 {
            let actual = &args[0];
            let expected = &args[1];
            if actual != expected {
                let msg = args.get(2).map(|a| a.as_string()).unwrap_or_else(|| {
                    format!(
                        "Assertion failed: expected {}, got {}",
                        expected.as_string(),
                        actual.as_string()
                    )
                });
                return Err(RuntimeError::thrown(msg));
            }
            Ok(Value::Nil)
        } else {
            Err(RuntimeError::thrown(
                "assert_eq requires at least 2 arguments".to_string(),
            ))
        }
    });

    interp.register_builtin("assert_ne", |args, _out| {
        if args.len() >= 2 {
            let actual = &args[0];
            let expected = &args[1];
            if actual == expected {
                let msg = args.get(2).map(|a| a.as_string()).unwrap_or_else(|| {
                    format!(
                        "Assertion failed: values should not be equal: {}",
                        actual.as_string()
                    )
                });
                return Err(RuntimeError::thrown(msg));
            }
            Ok(Value::Nil)
        } else {
            Err(RuntimeError::thrown(
                "assert_ne requires at least 2 arguments".to_string(),
            ))
        }
    });

    interp.register_builtin("regex_replace", |args, _out| {
        if args.len() >= 3 {
            let pattern = args[0].as_string();
            let replacement = args[1].as_string();
            let text = args[2].as_string();
            let re = regex::Regex::new(&pattern)
                .map_err(|e| RuntimeError::thrown(format!("Invalid regex: {e}")))?;
            return Ok(Value::String(
                re.replace_all(&text, replacement.as_str()).into_owned(),
            ));
        }
        Ok(Value::Nil)
    });

    // --- File system builtins ---

    interp.register_builtin("file_exists", |args, _out| {
        let path = args.first().map(|a| a.as_string()).unwrap_or_default();
        Ok(Value::Bool(std::path::Path::new(&path).exists()))
    });

    interp.register_builtin("delete_file", |args, _out| {
        let path = args.first().map(|a| a.as_string()).unwrap_or_default();
        let p = std::path::Path::new(&path);
        if p.is_dir() {
            std::fs::remove_dir_all(&path).map_err(|e| {
                RuntimeError::thrown(format!("Failed to delete directory {path}: {e}"))
            })?;
        } else {
            std::fs::remove_file(&path)
                .map_err(|e| RuntimeError::thrown(format!("Failed to delete file {path}: {e}")))?;
        }
        Ok(Value::Nil)
    });

    interp.register_builtin("list_dir", |args, _out| {
        let path = args
            .first()
            .map(|a| a.as_string())
            .unwrap_or_else(|| ".".to_string());
        let entries = std::fs::read_dir(&path)
            .map_err(|e| RuntimeError::thrown(format!("Failed to list directory {path}: {e}")))?;
        let mut result = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| RuntimeError::thrown(e.to_string()))?;
            let name = entry.file_name().to_string_lossy().to_string();
            result.push(Value::String(name));
        }
        result.sort_by_key(|a| a.as_string());
        Ok(Value::List(result))
    });

    interp.register_builtin("mkdir", |args, _out| {
        let path = args.first().map(|a| a.as_string()).unwrap_or_default();
        std::fs::create_dir_all(&path)
            .map_err(|e| RuntimeError::thrown(format!("Failed to create directory {path}: {e}")))?;
        Ok(Value::Nil)
    });

    interp.register_builtin("path_join", |args, _out| {
        let mut path = std::path::PathBuf::new();
        for arg in args {
            path.push(arg.as_string());
        }
        Ok(Value::String(path.to_string_lossy().to_string()))
    });

    interp.register_builtin("copy_file", |args, _out| {
        if args.len() >= 2 {
            let src = args[0].as_string();
            let dst = args[1].as_string();
            std::fs::copy(&src, &dst)
                .map_err(|e| RuntimeError::thrown(format!("Failed to copy {src} to {dst}: {e}")))?;
        }
        Ok(Value::Nil)
    });

    interp.register_builtin("append_file", |args, _out| {
        use std::io::Write;
        if args.len() >= 2 {
            let path = args[0].as_string();
            let content = args[1].as_string();
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)
                .map_err(|e| RuntimeError::thrown(format!("Failed to open file {path}: {e}")))?;
            file.write_all(content.as_bytes()).map_err(|e| {
                RuntimeError::thrown(format!("Failed to append to file {path}: {e}"))
            })?;
        }
        Ok(Value::Nil)
    });

    interp.register_builtin("temp_dir", |_args, _out| {
        Ok(Value::String(
            std::env::temp_dir().to_string_lossy().to_string(),
        ))
    });

    interp.register_builtin("stat", |args, _out| {
        let path = args.first().map(|a| a.as_string()).unwrap_or_default();
        let metadata = std::fs::metadata(&path)
            .map_err(|e| RuntimeError::thrown(format!("Failed to stat {path}: {e}")))?;
        let mut info = std::collections::BTreeMap::new();
        info.insert("size".to_string(), Value::Int(metadata.len() as i64));
        info.insert("is_file".to_string(), Value::Bool(metadata.is_file()));
        info.insert("is_dir".to_string(), Value::Bool(metadata.is_dir()));
        info.insert(
            "readonly".to_string(),
            Value::Bool(metadata.permissions().readonly()),
        );
        if let Ok(modified) = metadata.modified() {
            if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                info.insert("modified".to_string(), Value::Float(dur.as_secs_f64()));
            }
        }
        Ok(Value::Dict(info))
    });

    // --- Process execution builtins ---

    interp.register_builtin("exec", |args, _out| {
        if args.is_empty() {
            return Err(RuntimeError::thrown("exec: command is required"));
        }
        let cmd = args[0].as_string();
        let cmd_args: Vec<String> = args[1..].iter().map(|a| a.as_string()).collect();
        let output = std::process::Command::new(&cmd)
            .args(&cmd_args)
            .output()
            .map_err(|e| RuntimeError::thrown(format!("exec failed: {e}")))?;
        Ok(output_to_value(output))
    });

    interp.register_builtin("shell", |args, _out| {
        let cmd = args.first().map(|a| a.as_string()).unwrap_or_default();
        if cmd.is_empty() {
            return Err(RuntimeError::thrown("shell: command string is required"));
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
            .map_err(|e| RuntimeError::thrown(format!("shell failed: {e}")))?;
        Ok(output_to_value(output))
    });

    // --- Date/time builtins ---

    interp.register_builtin("date_now", |_args, _out| {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let total_secs = now.as_secs();
        let (y, m, d, hour, minute, second, dow) = civil_from_timestamp(total_secs);
        let mut result = std::collections::BTreeMap::new();
        result.insert("year".to_string(), Value::Int(y));
        result.insert("month".to_string(), Value::Int(m));
        result.insert("day".to_string(), Value::Int(d));
        result.insert("hour".to_string(), Value::Int(hour));
        result.insert("minute".to_string(), Value::Int(minute));
        result.insert("second".to_string(), Value::Int(second));
        result.insert("weekday".to_string(), Value::Int(dow));
        result.insert("timestamp".to_string(), Value::Float(now.as_secs_f64()));
        Ok(Value::Dict(result))
    });

    interp.register_builtin("date_format", |args, _out| {
        // date_format(timestamp, format_str)
        // Format tokens: %Y=year, %m=month, %d=day, %H=hour, %M=minute, %S=second
        let ts = match args.first() {
            Some(Value::Float(f)) => *f,
            Some(Value::Int(n)) => *n as f64,
            Some(Value::Dict(map)) => map
                .get("timestamp")
                .and_then(|v| match v {
                    Value::Float(f) => Some(*f),
                    Value::Int(n) => Some(*n as f64),
                    _ => None,
                })
                .unwrap_or(0.0),
            _ => 0.0,
        };
        let fmt = args
            .get(1)
            .map(|a| a.as_string())
            .unwrap_or_else(|| "%Y-%m-%d %H:%M:%S".to_string());

        let (y, m, d, hour, minute, second, _dow) = civil_from_timestamp(ts as u64);

        let result = fmt
            .replace("%Y", &format!("{y:04}"))
            .replace("%m", &format!("{m:02}"))
            .replace("%d", &format!("{d:02}"))
            .replace("%H", &format!("{hour:02}"))
            .replace("%M", &format!("{minute:02}"))
            .replace("%S", &format!("{second:02}"));

        Ok(Value::String(result))
    });

    interp.register_builtin("date_parse", |args, _out| {
        // date_parse("2024-01-15 10:30:00") -> timestamp float
        // Simple parser for "%Y-%m-%d %H:%M:%S" and "%Y-%m-%d"
        let s = args.first().map(|a| a.as_string()).unwrap_or_default();
        let parts: Vec<&str> = s.split(|c: char| !c.is_ascii_digit()).collect();
        let parts: Vec<i64> = parts.iter().filter_map(|p| p.parse().ok()).collect();
        if parts.len() < 3 {
            return Err(RuntimeError::thrown(format!("Cannot parse date: {s}")));
        }
        let (y, m, d) = (parts[0], parts[1], parts[2]);
        let hour = parts.get(3).copied().unwrap_or(0);
        let minute = parts.get(4).copied().unwrap_or(0);
        let second = parts.get(5).copied().unwrap_or(0);

        // Convert to days since epoch (inverse of civil_from_days)
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
        Ok(Value::Float(ts as f64))
    });

    // --- String formatting ---

    interp.register_builtin("format", |args, _out| {
        // format("Hello, {}! You are {} years old.", name, age)
        let template = args.first().map(|a| a.as_string()).unwrap_or_default();
        let mut result = String::with_capacity(template.len());
        let mut arg_iter = args.iter().skip(1);
        let mut rest = template.as_str();
        while let Some(pos) = rest.find("{}") {
            result.push_str(&rest[..pos]);
            if let Some(arg) = arg_iter.next() {
                result.push_str(&arg.as_string());
            } else {
                result.push_str("{}");
            }
            rest = &rest[pos + 2..];
        }
        result.push_str(rest);
        Ok(Value::String(result))
    });
}

fn escape_json_string(s: &str) -> String {
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

fn value_to_json(val: &Value) -> String {
    match val {
        Value::String(s) => escape_json_string(s),
        Value::Int(n) => n.to_string(),
        Value::Float(n) => {
            if n.is_finite() {
                n.to_string()
            } else {
                "null".to_string()
            }
        }
        Value::Bool(b) => b.to_string(),
        Value::Nil => "null".to_string(),
        Value::List(items) => {
            let inner: Vec<String> = items.iter().map(value_to_json).collect();
            format!("[{}]", inner.join(","))
        }
        Value::Dict(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}:{}", escape_json_string(k), value_to_json(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        _ => "null".to_string(),
    }
}

/// Convert a Unix timestamp (seconds) to civil date components (UTC).
/// Returns (year, month, day, hour, minute, second, weekday).
/// Weekday: 0=Sunday, 1=Monday, ..., 6=Saturday.
/// Uses Howard Hinnant's civil_from_days algorithm.
fn civil_from_timestamp(total_secs: u64) -> (i64, i64, i64, i64, i64, i64, i64) {
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
