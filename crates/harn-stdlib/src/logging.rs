use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use harn_runtime::{Interpreter, RuntimeError, Value};
use rand::Rng;

// Log level encoding: debug=0, info=1, warn=2, error=3
static MIN_LOG_LEVEL: AtomicU8 = AtomicU8::new(0);

#[derive(Clone)]
struct TraceContext {
    trace_id: String,
    span_id: String,
}

thread_local! {
    static TRACE_STACK: RefCell<Vec<TraceContext>> = const { RefCell::new(Vec::new()) };
}

fn level_to_u8(level: &str) -> Option<u8> {
    match level {
        "debug" => Some(0),
        "info" => Some(1),
        "warn" => Some(2),
        "error" => Some(3),
        _ => None,
    }
}

fn gen_hex_id() -> String {
    let val: u32 = rand::thread_rng().gen();
    format!("{val:08x}")
}

fn format_timestamp_utc() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = now.as_secs();
    let millis = now.subsec_millis();

    let days = total_secs / 86400;
    let time_of_day = total_secs % 86400;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    // Howard Hinnant civil_from_days
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

fn escape_json_str(s: &str) -> String {
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

fn value_to_json_fragment(val: &Value) -> String {
    match val {
        Value::String(s) => escape_json_str(s),
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
        _ => escape_json_str(&val.as_string()),
    }
}

fn build_log_line(level: &str, msg: &str, fields: Option<&BTreeMap<String, Value>>) -> String {
    let ts = format_timestamp_utc();
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("\"ts\":{}", escape_json_str(&ts)));
    parts.push(format!("\"level\":{}", escape_json_str(level)));
    parts.push(format!("\"msg\":{}", escape_json_str(msg)));

    // Add trace context if active
    TRACE_STACK.with(|stack| {
        if let Some(trace) = stack.borrow().last() {
            parts.push(format!("\"trace_id\":{}", escape_json_str(&trace.trace_id)));
            parts.push(format!("\"span_id\":{}", escape_json_str(&trace.span_id)));
        }
    });

    // Merge user-supplied fields
    if let Some(dict) = fields {
        for (k, v) in dict {
            parts.push(format!(
                "{}:{}",
                escape_json_str(k),
                value_to_json_fragment(v)
            ));
        }
    }

    format!("{{{}}}\n", parts.join(","))
}

fn write_log(level: &str, level_num: u8, args: &[Value], out: &mut Vec<u8>) {
    if level_num < MIN_LOG_LEVEL.load(Ordering::Relaxed) {
        return;
    }
    let msg = args.first().map(|a| a.as_string()).unwrap_or_default();
    let fields = args.get(1).and_then(|v| {
        if let Value::Dict(d) = v {
            Some(d)
        } else {
            None
        }
    });
    let line = build_log_line(level, &msg, fields);
    out.extend_from_slice(line.as_bytes());
}

/// Register all structured logging builtins on an interpreter.
pub fn register_logging_builtins(interp: &mut Interpreter) {
    // Reset global state for clean per-program execution
    MIN_LOG_LEVEL.store(0, Ordering::Relaxed);
    TRACE_STACK.with(|stack| {
        stack.borrow_mut().clear();
    });
    // --- Structured log level builtins ---

    interp.register_builtin("log_debug", |args, out| {
        write_log("debug", 0, args, out);
        Ok(Value::Nil)
    });

    interp.register_builtin("log_info", |args, out| {
        write_log("info", 1, args, out);
        Ok(Value::Nil)
    });

    interp.register_builtin("log_warn", |args, out| {
        write_log("warn", 2, args, out);
        Ok(Value::Nil)
    });

    interp.register_builtin("log_error", |args, out| {
        write_log("error", 3, args, out);
        Ok(Value::Nil)
    });

    // --- Log level filtering ---

    interp.register_builtin("log_set_level", |args, _out| {
        let level_str = args.first().map(|a| a.as_string()).unwrap_or_default();
        match level_to_u8(&level_str) {
            Some(n) => {
                MIN_LOG_LEVEL.store(n, Ordering::Relaxed);
                Ok(Value::Nil)
            }
            None => Err(RuntimeError::thrown(format!(
                "log_set_level: invalid level '{}'. Expected debug, info, warn, or error",
                level_str
            ))),
        }
    });

    // --- Trace context builtins ---

    interp.register_builtin("trace_start", |args, _out| {
        let name = args.first().map(|a| a.as_string()).unwrap_or_default();
        let trace_id = TRACE_STACK.with(|stack| {
            stack
                .borrow()
                .last()
                .map(|t| t.trace_id.clone())
                .unwrap_or_else(gen_hex_id)
        });
        let span_id = gen_hex_id();
        let start_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        // Push trace context onto stack (supports nesting)
        TRACE_STACK.with(|stack| {
            stack.borrow_mut().push(TraceContext {
                trace_id: trace_id.clone(),
                span_id: span_id.clone(),
            });
        });

        let mut span = BTreeMap::new();
        span.insert("trace_id".to_string(), Value::String(trace_id));
        span.insert("span_id".to_string(), Value::String(span_id));
        span.insert("name".to_string(), Value::String(name));
        span.insert("start_ms".to_string(), Value::Int(start_ms));
        Ok(Value::Dict(span))
    });

    interp.register_builtin("trace_end", |args, out| {
        let span = match args.first() {
            Some(Value::Dict(d)) => d,
            _ => {
                return Err(RuntimeError::thrown(
                    "trace_end: argument must be a span dict from trace_start",
                ));
            }
        };

        let end_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let start_ms = span
            .get("start_ms")
            .and_then(|v| v.as_int())
            .unwrap_or(end_ms);
        let duration_ms = end_ms - start_ms;
        let name = span.get("name").map(|v| v.as_string()).unwrap_or_default();
        let trace_id = span
            .get("trace_id")
            .map(|v| v.as_string())
            .unwrap_or_default();
        let span_id = span
            .get("span_id")
            .map(|v| v.as_string())
            .unwrap_or_default();

        // Pop this span from the stack (restores parent span context)
        TRACE_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });

        // Log span completion event (respects log level filtering)
        let level_num = 1_u8; // info level
        if level_num >= MIN_LOG_LEVEL.load(Ordering::Relaxed) {
            let mut fields = BTreeMap::new();
            fields.insert("trace_id".to_string(), Value::String(trace_id));
            fields.insert("span_id".to_string(), Value::String(span_id));
            fields.insert("name".to_string(), Value::String(name));
            fields.insert("duration_ms".to_string(), Value::Int(duration_ms));
            let line = build_log_line("info", "span_end", Some(&fields));
            out.extend_from_slice(line.as_bytes());
        }

        Ok(Value::Nil)
    });

    interp.register_builtin("trace_id", |_args, _out| {
        let id = TRACE_STACK.with(|stack| stack.borrow().last().map(|t| t.trace_id.clone()));
        match id {
            Some(trace_id) => Ok(Value::String(trace_id)),
            None => Ok(Value::Nil),
        }
    });
}
