use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;

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
        let mut s = stack.borrow_mut();
        if let Some(top) = s.last() {
            if top.span_id == span_id {
                s.pop();
            }
        }
    });

    Ok((name, trace_id, span_id, duration_ms))
}

pub(crate) fn register_tracing_builtins(vm: &mut Vm) {
    vm.register_builtin("trace_start", |args, _out| {
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

        let mut span = BTreeMap::new();
        span.insert("trace_id".to_string(), VmValue::String(Rc::from(trace_id)));
        span.insert("span_id".to_string(), VmValue::String(Rc::from(span_id)));
        span.insert("name".to_string(), VmValue::String(Rc::from(name)));
        span.insert("start_ms".to_string(), VmValue::Int(start_ms));
        Ok(VmValue::Dict(Rc::new(span)))
    });

    vm.register_builtin("trace_end", |args, out| {
        let (name, trace_id, span_id, duration_ms) = finish_span_from_args(args)?;
        let level_num = 1_u8;
        if level_num >= VM_MIN_LOG_LEVEL.load(Ordering::Relaxed) {
            let mut fields = BTreeMap::new();
            fields.insert("trace_id".to_string(), VmValue::String(Rc::from(trace_id)));
            fields.insert("span_id".to_string(), VmValue::String(Rc::from(span_id)));
            fields.insert("name".to_string(), VmValue::String(Rc::from(name)));
            fields.insert("duration_ms".to_string(), VmValue::Int(duration_ms));
            let line = vm_build_log_line("info", "span_end", Some(&fields));
            out.push_str(&line);
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("trace_id", |_args, _out| {
        let id = VM_TRACE_STACK.with(|stack| stack.borrow().last().map(|t| t.trace_id.clone()));
        match id {
            Some(trace_id) => Ok(VmValue::String(Rc::from(trace_id))),
            None => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("llm_info", |_args, _out| {
        let provider = std::env::var("HARN_LLM_PROVIDER").unwrap_or_default();
        let model = std::env::var("HARN_LLM_MODEL").unwrap_or_default();
        let api_key_set = std::env::var("HARN_API_KEY")
            .or_else(|_| std::env::var("OPENROUTER_API_KEY"))
            .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
            .is_ok();
        let mut info = BTreeMap::new();
        info.insert("provider".to_string(), VmValue::String(Rc::from(provider)));
        info.insert("model".to_string(), VmValue::String(Rc::from(model)));
        info.insert("api_key_set".to_string(), VmValue::Bool(api_key_set));
        Ok(VmValue::Dict(Rc::new(info)))
    });

    vm.register_builtin("enable_tracing", |args, _out| {
        let enabled = match args.first() {
            Some(VmValue::Bool(b)) => *b,
            _ => true,
        };
        crate::tracing::set_tracing_enabled(enabled);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("trace_spans", |_args, _out| {
        let spans = crate::tracing::peek_spans();
        let vm_spans: Vec<VmValue> = spans.iter().map(crate::tracing::span_to_vm_value).collect();
        Ok(VmValue::List(Rc::new(vm_spans)))
    });

    vm.register_builtin("trace_summary", |_args, _out| {
        Ok(VmValue::String(Rc::from(crate::tracing::format_summary())))
    });

    vm.register_builtin("llm_usage", |_args, _out| {
        let (total_input, total_output, total_duration, call_count) =
            crate::llm::peek_trace_summary();
        let mut usage = BTreeMap::new();
        usage.insert("input_tokens".to_string(), VmValue::Int(total_input));
        usage.insert("output_tokens".to_string(), VmValue::Int(total_output));
        usage.insert(
            "total_duration_ms".to_string(),
            VmValue::Int(total_duration),
        );
        usage.insert("call_count".to_string(), VmValue::Int(call_count));
        usage.insert("total_calls".to_string(), VmValue::Int(call_count));
        Ok(VmValue::Dict(Rc::new(usage)))
    });
}
