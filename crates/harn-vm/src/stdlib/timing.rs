//! Builtins for `std/timing` — scoped duration spans.
//!
//! `std/timing` promotes the hand-rolled `let started_ms = now_ms();
//! work(); let dur = now_ms() - started_ms;` pattern into a first-class
//! observability primitive. The builtins here are thin wrappers over the
//! VM span collector: they always record (regardless of
//! `enable_tracing(true)`) so scripts get `duration_ms` back even when
//! the broader trace tree is disabled, while still slotting into the
//! tracing hierarchy when callers opt into profile/OTel export.
//!
//! Three builtins back the public API:
//!
//! - `__timing_start(name, attrs)` opens a span and returns the handle.
//! - `__timing_event(handle, name, attrs)` records a sub-phase event on
//!   the open span without nesting a child span for every marker.
//! - `__timing_end(handle, final_attrs)` finalizes, merges final
//!   attributes (including `status` / `error`), and returns the
//!   `TimingSegment` dict with `duration_ms` plus the OTel-style trace
//!   identifiers.
//!
//! Duplicate `end` calls are idempotent — the second call returns the
//! result from the first close rather than corrupting the active span
//! stack — and missing handles surface as a typed thrown error so
//! callers never silently drop work.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Instant;

use crate::llm::helpers::vm_value_to_json;
use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

/// Process-wide monotonic anchor for unmocked timing handles. Mirrors
/// the stdlib clock module's anchor but avoids its testbench tape
/// recorder — span lifecycle should not look like a script clock read.
static TIMING_MONOTONIC_START: OnceLock<Instant> = OnceLock::new();

fn now_monotonic_ms() -> i64 {
    if let Some(mock) = crate::clock_mock::active_mock_clock() {
        return mock.now_monotonic_ms();
    }
    let anchor = TIMING_MONOTONIC_START.get_or_init(Instant::now);
    anchor.elapsed().as_millis() as i64
}

/// Finalized handle that `__timing_end` returns. Kept in a thread-local
/// cache so duplicate-close callers can read back the original timing
/// result instead of producing nonsense duration values. The `trace_id`
/// guards against stale entries surviving a `reset_tracing()` — the VM
/// span collector reuses `span_id`s from 1 after reset, but each new
/// trace gets a fresh `trace_id`, so a mismatch tells us the cached
/// entry belongs to an earlier trace and should be ignored.
#[derive(Clone)]
struct FinalizedTiming {
    trace_id: String,
    value: serde_json::Value,
}

thread_local! {
    static FINALIZED: RefCell<BTreeMap<u64, FinalizedTiming>> =
        const { RefCell::new(BTreeMap::new()) };
}

pub(crate) fn reset_timing_state() {
    FINALIZED.with(|state| state.borrow_mut().clear());
}

pub(crate) fn register_timing_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "__timing_start(name: string, attributes?: dict) -> dict",
    category = "timing"
)]
fn timing_start_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let name = string_arg(args.first(), "__timing_start", "name")?;
    let attrs = object_arg(args.get(1), "__timing_start", "attributes")?;
    let attrs_btree = json_map_to_btree(&attrs);
    let (span_id, trace_id, parent_id, start_unix_ms) =
        crate::tracing::span_start_user_timing(name.clone(), attrs_btree);

    let mut handle = BTreeMap::new();
    handle.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
    handle.insert("span_id".to_string(), VmValue::Int(span_id as i64));
    handle.insert(
        "trace_id".to_string(),
        VmValue::String(Rc::from(trace_id.as_str())),
    );
    handle.insert(
        "parent_span_id".to_string(),
        parent_id
            .map(|id| VmValue::Int(id as i64))
            .unwrap_or(VmValue::Nil),
    );
    handle.insert(
        "started_at_ms".to_string(),
        VmValue::Int(start_unix_ms as i64),
    );
    handle.insert(
        "monotonic_started_ms".to_string(),
        VmValue::Int(now_monotonic_ms()),
    );
    handle.insert(
        "attributes".to_string(),
        json_to_vm_value(&serde_json::Value::Object(attrs)),
    );
    handle.insert(
        "kind".to_string(),
        VmValue::String(Rc::from(crate::tracing::SpanKind::UserTiming.as_str())),
    );

    Ok(VmValue::Dict(Rc::new(handle)))
}

#[harn_builtin(sig = "__timing_now_monotonic_ms() -> int", category = "timing")]
fn timing_now_monotonic_ms_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Int(now_monotonic_ms()))
}

#[harn_builtin(
    sig = "__timing_event(handle: dict, name: string, attributes?: dict) -> bool",
    category = "timing"
)]
fn timing_event_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let span_id = handle_span_id(args.first(), "__timing_event")?;
    let name = string_arg(args.get(1), "__timing_event", "name")?;
    let attrs = object_arg(args.get(2), "__timing_event", "attributes")?;
    let attached = crate::tracing::span_record_event(span_id, name, json_map_to_btree(&attrs));
    Ok(VmValue::Bool(attached))
}

#[harn_builtin(
    sig = "__timing_end(handle: dict, final_attributes?: dict) -> dict",
    category = "timing"
)]
fn timing_end_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let span_id = handle_span_id(args.first(), "__timing_end")?;
    let handle_trace_id = handle_trace_id(args.first());
    let final_attrs = object_arg(args.get(1), "__timing_end", "final_attributes")?;

    // Idempotency: if this handle was already closed under the same
    // trace, return the cached result instead of mutating the span
    // stack again. A mismatched trace_id means `reset_tracing()`
    // wrapped around the span_id counter, so the cached entry is
    // stale and should not satisfy the lookup.
    let cached = FINALIZED.with(|state| {
        state
            .borrow()
            .get(&span_id)
            .filter(|entry| match handle_trace_id.as_deref() {
                Some(trace) => entry.trace_id == trace,
                None => true,
            })
            .cloned()
    });
    if let Some(cached) = cached {
        return Ok(json_to_vm_value(&cached.value));
    }

    for (key, value) in &final_attrs {
        crate::tracing::span_attach_metadata(span_id, key, value.clone());
    }

    let Some(closed) = crate::tracing::span_end(span_id) else {
        return Err(VmError::Runtime(format!(
            "__timing_end: unknown timing handle (span_id={span_id})"
        )));
    };

    let trace_id = closed.trace_id.clone();
    let payload = finalized_to_json(&closed);
    FINALIZED.with(|state| {
        state.borrow_mut().insert(
            span_id,
            FinalizedTiming {
                trace_id,
                value: payload.clone(),
            },
        );
    });
    Ok(json_to_vm_value(&payload))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &TIMING_START_IMPL_DEF,
    &TIMING_NOW_MONOTONIC_MS_IMPL_DEF,
    &TIMING_EVENT_IMPL_DEF,
    &TIMING_END_IMPL_DEF,
];

fn finalized_to_json(span: &crate::tracing::Span) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    out.insert(
        "name".to_string(),
        serde_json::Value::String(span.name.clone()),
    );
    out.insert(
        "trace_id".to_string(),
        serde_json::Value::String(span.trace_id.clone()),
    );
    out.insert(
        "span_id".to_string(),
        serde_json::Value::Number(serde_json::Number::from(span.span_id)),
    );
    out.insert(
        "parent_span_id".to_string(),
        span.parent_id
            .map(|id| serde_json::Value::Number(serde_json::Number::from(id)))
            .unwrap_or(serde_json::Value::Null),
    );
    out.insert(
        "kind".to_string(),
        serde_json::Value::String(span.kind.as_str().to_string()),
    );
    out.insert(
        "started_at_ms".to_string(),
        serde_json::Value::Number(serde_json::Number::from(span.start_unix_ms)),
    );
    out.insert(
        "monotonic_started_ms".to_string(),
        serde_json::Value::Number(serde_json::Number::from(span.start_ms)),
    );
    out.insert(
        "ended_at_ms".to_string(),
        serde_json::Value::Number(serde_json::Number::from(
            span.start_unix_ms.saturating_add(span.duration_ms),
        )),
    );
    out.insert(
        "duration_ms".to_string(),
        serde_json::Value::Number(serde_json::Number::from(span.duration_ms)),
    );

    // Promote status/error to dedicated keys so callers do not have to
    // dig through `metadata.status` for the common case, but keep the
    // full metadata bag available too.
    let metadata = serde_json::Value::Object(
        span.metadata
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    );
    out.insert(
        "attributes".to_string(),
        attributes_from_metadata(&span.metadata),
    );
    if let Some(status) = span
        .metadata
        .get("status")
        .and_then(serde_json::Value::as_str)
    {
        out.insert(
            "status".to_string(),
            serde_json::Value::String(status.to_string()),
        );
    }
    if let Some(error) = span
        .metadata
        .get("error")
        .and_then(serde_json::Value::as_str)
    {
        out.insert(
            "error".to_string(),
            serde_json::Value::String(error.to_string()),
        );
    }
    out.insert("metadata".to_string(), metadata);
    if !span.events.is_empty() {
        out.insert("events".to_string(), serde_json::json!(span.events));
    }
    serde_json::Value::Object(out)
}

fn attributes_from_metadata(metadata: &BTreeMap<String, serde_json::Value>) -> serde_json::Value {
    serde_json::Value::Object(
        metadata
            .iter()
            .filter(|(key, _)| key.as_str() != "status" && key.as_str() != "error")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

fn string_arg(value: Option<&VmValue>, builtin: &str, field: &str) -> Result<String, VmError> {
    match value {
        Some(VmValue::String(raw)) => Ok(raw.to_string()),
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: {field} must be a string, got {}",
            other.type_name()
        ))),
        None => Err(VmError::Runtime(format!("{builtin}: missing {field}"))),
    }
}

fn object_arg(
    value: Option<&VmValue>,
    builtin: &str,
    field: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(serde_json::Map::new()),
        Some(VmValue::Dict(_)) => {
            let json = vm_value_to_json(value.expect("checked above"));
            match json {
                serde_json::Value::Object(map) => Ok(map),
                _ => unreachable!("Dict converts to Object"),
            }
        }
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: {field} must be a dict, got {}",
            other.type_name()
        ))),
    }
}

fn handle_trace_id(value: Option<&VmValue>) -> Option<String> {
    let VmValue::Dict(map) = value? else {
        return None;
    };
    match map.get("trace_id")? {
        VmValue::String(raw) => Some(raw.to_string()),
        _ => None,
    }
}

fn handle_span_id(value: Option<&VmValue>, builtin: &str) -> Result<u64, VmError> {
    let Some(VmValue::Dict(map)) = value else {
        return Err(VmError::Runtime(format!(
            "{builtin}: handle must be the dict returned by start_timing"
        )));
    };
    let raw = map
        .get("span_id")
        .and_then(VmValue::as_int)
        .ok_or_else(|| {
            VmError::Runtime(format!(
                "{builtin}: handle is missing span_id (was it returned by start_timing?)"
            ))
        })?;
    if raw < 0 {
        return Err(VmError::Runtime(format!(
            "{builtin}: handle.span_id must be non-negative, got {raw}"
        )));
    }
    Ok(raw as u64)
}

fn json_map_to_btree(
    map: &serde_json::Map<String, serde_json::Value>,
) -> BTreeMap<String, serde_json::Value> {
    map.iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}
