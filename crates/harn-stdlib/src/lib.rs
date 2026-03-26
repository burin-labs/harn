#![allow(clippy::result_large_err)]

mod async_builtins;
mod json;
mod llm;

use harn_runtime::{Interpreter, RuntimeError, Value};

pub use async_builtins::register_async_builtins;
pub use llm::register_llm_builtins;

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
            Value::Float(n) => Ok(Value::Int(n.floor() as i64)),
            Value::Int(n) => Ok(Value::Int(*n)),
            _ => Ok(Value::Nil),
        }
    });

    interp.register_builtin("ceil", |args, _out| {
        match args.first().unwrap_or(&Value::Nil) {
            Value::Float(n) => Ok(Value::Int(n.ceil() as i64)),
            Value::Int(n) => Ok(Value::Int(*n)),
            _ => Ok(Value::Nil),
        }
    });

    interp.register_builtin("round", |args, _out| {
        match args.first().unwrap_or(&Value::Nil) {
            Value::Float(n) => Ok(Value::Int(n.round() as i64)),
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
