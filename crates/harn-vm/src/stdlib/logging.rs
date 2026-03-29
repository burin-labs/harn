use std::collections::BTreeMap;
use std::sync::atomic::AtomicU8;

use crate::value::VmValue;

pub(crate) static VM_MIN_LOG_LEVEL: AtomicU8 = AtomicU8::new(0);

#[derive(Clone)]
pub(crate) struct VmTraceContext {
    pub(crate) trace_id: String,
    pub(crate) span_id: String,
}

thread_local! {
    pub(crate) static VM_TRACE_STACK: std::cell::RefCell<Vec<VmTraceContext>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Reset thread-local logging state. Call between test runs.
pub(crate) fn reset_logging_state() {
    VM_MIN_LOG_LEVEL.store(0, std::sync::atomic::Ordering::Relaxed);
    VM_TRACE_STACK.with(|s| s.borrow_mut().clear());
}

pub(crate) fn vm_level_to_u8(level: &str) -> Option<u8> {
    match level {
        "debug" => Some(0),
        "info" => Some(1),
        "warn" => Some(2),
        "error" => Some(3),
        _ => None,
    }
}

pub(crate) fn vm_format_timestamp_utc() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = now.as_secs();
    let millis = now.subsec_millis();

    let (y, m, d, hour, minute, second, _) = super::datetime::vm_civil_from_timestamp(total_secs);

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

pub(crate) fn vm_escape_json_str_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&vm_escape_json_str(s));
    out.push('"');
    out
}

pub(crate) fn vm_value_to_json_fragment(val: &VmValue) -> String {
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

pub(crate) fn vm_build_log_line(
    level: &str,
    msg: &str,
    fields: Option<&BTreeMap<String, VmValue>>,
) -> String {
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
