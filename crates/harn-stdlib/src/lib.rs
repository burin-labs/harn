use harn_runtime::{Interpreter, RuntimeError, Value};

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
                if n.is_finite() && *n >= i64::MIN as f64 && *n <= i64::MAX as f64 {
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

    interp.register_builtin("json_parse", |_args, _out| {
        // Stub — requires a full JSON parser for production use
        Ok(Value::Nil)
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

    interp.register_builtin("sleep", |_args, _out| {
        // No-op in sync interpreter — async runtime needed for real sleep
        Ok(Value::Nil)
    });

    interp.register_builtin("read_file", |args, _out| {
        let path = args.first().map(|a| a.as_string()).unwrap_or_default();
        match std::fs::read_to_string(&path) {
            Ok(content) => Ok(Value::String(content)),
            Err(e) => Err(RuntimeError::ThrownError(Value::String(format!(
                "Failed to read file {path}: {e}"
            )))),
        }
    });

    interp.register_builtin("write_file", |args, _out| {
        if args.len() >= 2 {
            let path = args[0].as_string();
            let content = args[1].as_string();
            std::fs::write(&path, &content).map_err(|e| {
                RuntimeError::ThrownError(Value::String(format!(
                    "Failed to write file {path}: {e}"
                )))
            })?;
        }
        Ok(Value::Nil)
    });

    interp.register_builtin("exit", |args, _out| {
        let code = args.first().and_then(|a| a.as_int()).unwrap_or(0);
        std::process::exit(code as i32);
    });

    interp.register_builtin("regex_match", |_args, _out| {
        // Stub — requires regex crate for production use
        Ok(Value::Nil)
    });

    interp.register_builtin("regex_replace", |args, _out| {
        // Stub — returns text unchanged
        if args.len() >= 3 {
            return Ok(Value::String(args[2].as_string()));
        }
        Ok(Value::Nil)
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
        Value::Float(n) => n.to_string(),
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
