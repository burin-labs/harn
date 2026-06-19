use crate::value::VmDictExt;
use std::sync::atomic::Ordering;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::logging::{vm_build_log_line, VmTraceContext, VM_MIN_LOG_LEVEL, VM_TRACE_STACK};

/// Finish a span started by `trace_start`: computes the elapsed duration,
/// pops the span from the thread-local trace stack if it is on top, and
/// returns `(name, trace_id, span_id, duration_ms)` suitable for both the
/// default `out`-buffered `trace_end` and the bridge-streaming override
/// registered by the ACP runner.
pub fn finish_span_from_args(args: &[VmValue]) -> Result<(String, String, String, i64), VmError> {
    let span = match args.first() {
        Some(VmValue::Dict(d)) => d,
        _ => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
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
        let mut s = stack.borrow_mut();
        if let Some(top) = s.last() {
            if top.span_id == span_id {
                s.pop();
            }
        }
    });

    Ok((name, trace_id, span_id, duration_ms))
}

pub(crate) fn current_trace_context() -> Option<(String, String)> {
    VM_TRACE_STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .map(|trace| (trace.trace_id.clone(), trace.span_id.clone()))
    })
}

pub(crate) fn register_tracing_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "trace_start(...args: any) -> dict", category = "tracing")]
fn trace_start_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    use rand::RngExt;
    let name = args.first().map(|a| a.display()).unwrap_or_default();
    let trace_id = VM_TRACE_STACK.with(|stack| {
        stack
            .borrow()
            .last()
            .map(|t| t.trace_id.clone())
            .unwrap_or_else(|| {
                let val: u32 = rand::rng().random();
                format!("{val:08x}")
            })
    });
    let span_id = {
        let val: u32 = rand::rng().random();
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

    let mut span = crate::value::DictMap::new();
    span.put_str("trace_id", trace_id);
    span.put_str("span_id", span_id);
    span.put_str("name", name);
    span.insert(crate::value::intern_key("start_ms"), VmValue::Int(start_ms));
    Ok(VmValue::dict(span))
}

#[harn_builtin(sig = "trace_end(...args: any) -> nil", category = "tracing")]
fn trace_end_impl(args: &[VmValue], out: &mut String) -> Result<VmValue, VmError> {
    let (name, trace_id, span_id, duration_ms) = finish_span_from_args(args)?;
    let level_num = 1_u8;
    if level_num >= VM_MIN_LOG_LEVEL.load(Ordering::Relaxed) {
        let mut fields = crate::value::DictMap::new();
        fields.put_str("trace_id", trace_id);
        fields.put_str("span_id", span_id);
        fields.put_str("name", name);
        fields.insert(
            crate::value::intern_key("duration_ms"),
            VmValue::Int(duration_ms),
        );
        let line = vm_build_log_line("info", "span_end", Some(&fields));
        out.push_str(&line);
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(sig = "trace_id(...args: any) -> string?", category = "tracing")]
fn trace_id_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    match current_trace_context().map(|context| context.0) {
        Some(trace_id) => Ok(VmValue::String(arcstr::ArcStr::from(trace_id))),
        None => Ok(VmValue::Nil),
    }
}

#[harn_builtin(sig = "llm_info() -> dict", category = "tracing")]
fn llm_info_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let raw_model = std::env::var("HARN_LLM_MODEL").unwrap_or_default();
    let resolved = crate::llm_config::resolve_model_info(&raw_model);
    let provider = std::env::var("HARN_LLM_PROVIDER").unwrap_or(resolved.provider);
    let model = if raw_model.is_empty() {
        String::new()
    } else {
        resolved.id
    };
    let api_key_set = crate::llm_config::provider_key_available(&provider);
    let mut info = crate::value::DictMap::new();
    info.put_str("provider", provider);
    info.put_str("model", model);
    info.insert(
        crate::value::intern_key("api_key_set"),
        VmValue::Bool(api_key_set),
    );
    Ok(VmValue::dict(info))
}

#[harn_builtin(sig = "enable_tracing(enabled?: bool) -> nil", category = "tracing")]
fn enable_tracing_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let enabled = match args.first() {
        Some(VmValue::Bool(b)) => *b,
        _ => true,
    };
    crate::tracing::set_tracing_enabled(enabled);
    Ok(VmValue::Nil)
}

#[harn_builtin(sig = "trace_spans(...args: any) -> list", category = "tracing")]
fn trace_spans_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let spans = crate::tracing::peek_spans();
    let vm_spans: Vec<VmValue> = spans.iter().map(crate::tracing::span_to_vm_value).collect();
    Ok(VmValue::List(std::sync::Arc::new(vm_spans)))
}

#[harn_builtin(sig = "trace_summary(...args: any) -> string", category = "tracing")]
fn trace_summary_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::String(arcstr::ArcStr::from(
        crate::tracing::format_summary(),
    )))
}

#[harn_builtin(
    sig = "__lifecycle_span_start(name: string) -> int",
    category = "tracing"
)]
fn lifecycle_span_start_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let name = args.first().map(|a| a.display()).unwrap_or_default();
    let id = crate::tracing::span_start(crate::tracing::SpanKind::FnCall, name);
    Ok(VmValue::Int(id as i64))
}

#[harn_builtin(
    sig = "__lifecycle_span_end(span_id: int) -> nil",
    category = "tracing"
)]
fn lifecycle_span_end_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let span_id = match args.first().and_then(|v| v.as_int()) {
        Some(id) => id,
        None => return Ok(VmValue::Nil),
    };
    if span_id < 0 {
        return Ok(VmValue::Nil);
    }
    crate::tracing::span_end(span_id as u64);
    Ok(VmValue::Nil)
}

#[harn_builtin(sig = "llm_usage() -> dict", category = "tracing")]
fn llm_usage_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (total_input, total_output, total_duration, call_count) = crate::llm::peek_trace_summary();
    let mut usage = crate::value::DictMap::new();
    usage.insert(
        crate::value::intern_key("input_tokens"),
        VmValue::Int(total_input),
    );
    usage.insert(
        crate::value::intern_key("output_tokens"),
        VmValue::Int(total_output),
    );
    usage.insert(
        crate::value::intern_key("total_duration_ms"),
        VmValue::Int(total_duration),
    );
    usage.insert(
        crate::value::intern_key("call_count"),
        VmValue::Int(call_count),
    );
    usage.insert(
        crate::value::intern_key("total_calls"),
        VmValue::Int(call_count),
    );
    Ok(VmValue::dict(usage))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &TRACE_START_IMPL_DEF,
    &TRACE_END_IMPL_DEF,
    &TRACE_ID_IMPL_DEF,
    &LLM_INFO_IMPL_DEF,
    &ENABLE_TRACING_IMPL_DEF,
    &TRACE_SPANS_IMPL_DEF,
    &TRACE_SUMMARY_IMPL_DEF,
    &LIFECYCLE_SPAN_START_IMPL_DEF,
    &LIFECYCLE_SPAN_END_IMPL_DEF,
    &LLM_USAGE_IMPL_DEF,
];
