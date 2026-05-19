use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use crate::stdlib::json_to_vm_value;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::logging::{vm_build_log_line, VM_MIN_LOG_LEVEL};

const AUDIT_BACKEND_FAILURE: &str = "HARN-OBS-001";

#[derive(Clone, Debug)]
struct ObsSpan {
    id: String,
    trace_id: String,
    name: String,
    attrs: serde_json::Map<String, serde_json::Value>,
    vm_span_id: u64,
}

#[derive(Clone, Debug)]
struct ObsConfig {
    backend: serde_json::Value,
    backends: BTreeMap<String, serde_json::Value>,
    routes: Vec<serde_json::Value>,
    audit_to_pretty_stderr: bool,
}

impl Default for ObsConfig {
    fn default() -> Self {
        Self {
            backend: serde_json::json!({"kind": "auto", "id": "auto"}),
            backends: BTreeMap::new(),
            routes: Vec::new(),
            audit_to_pretty_stderr: true,
        }
    }
}

#[derive(Default)]
struct ObsState {
    config: ObsConfig,
    span_stack: Vec<ObsSpan>,
    next_span_id: u64,
    emissions: Vec<serde_json::Value>,
}

thread_local! {
    static OBS_STATE: RefCell<ObsState> = RefCell::new(ObsState::default());
}

pub(crate) fn reset_observability_state() {
    OBS_STATE.with(|state| *state.borrow_mut() = ObsState::default());
}

pub(crate) fn register_observability_builtins(vm: &mut Vm) {
    vm.register_builtin("__obs_configure", |args, _out| {
        let config_value = args.first().cloned().unwrap_or(VmValue::Nil);
        let parsed = parse_config(&config_value)?;
        OBS_STATE.with(|state| state.borrow_mut().config = parsed);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("__obs_reset", |_args, _out| {
        reset_observability_state();
        Ok(VmValue::Nil)
    });

    vm.register_builtin("__obs_start_span", |args, _out| {
        let name = string_arg(args.first(), "__obs_start_span", "name")?;
        let attrs = object_arg(args.get(1), "__obs_start_span", "attrs")?;
        let span = OBS_STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.next_span_id += 1;
            let parent = state.span_stack.last();
            let trace_id = parent
                .map(|span| span.trace_id.clone())
                .unwrap_or_else(|| format!("obs_trace_{}", uuid::Uuid::now_v7()));
            let span_id = format!("obs_span_{:016x}", state.next_span_id);
            let vm_span_id =
                crate::tracing::span_start(crate::tracing::SpanKind::FnCall, name.clone());
            for (key, value) in attrs.iter() {
                crate::tracing::span_set_metadata(vm_span_id, key, value.clone());
            }
            let span = ObsSpan {
                id: span_id,
                trace_id,
                name,
                attrs,
                vm_span_id,
            };
            state.span_stack.push(span.clone());
            span
        });
        Ok(span_to_vm_value(&span))
    });

    vm.register_builtin("__obs_end_span", |args, _out| {
        let span_id = field_string(args.first(), "span_id").unwrap_or_default();
        let ended = OBS_STATE.with(|state| {
            let mut state = state.borrow_mut();
            let pos = if span_id.is_empty() {
                state.span_stack.len().checked_sub(1)
            } else {
                state.span_stack.iter().rposition(|span| span.id == span_id)
            };
            pos.map(|idx| state.span_stack.remove(idx))
        });
        if let Some(span) = ended {
            crate::tracing::span_end(span.vm_span_id);
            let mut fields = serde_json::Map::new();
            fields.insert(
                "span_name".to_string(),
                serde_json::Value::String(span.name),
            );
            fields.extend(span.attrs);
            let mut event = base_event("span_end", "span", Some(fields), None, None)?;
            event.insert(
                "trace_id".to_string(),
                serde_json::Value::String(span.trace_id),
            );
            event.insert("span_id".to_string(), serde_json::Value::String(span.id));
            emit_event(event, None);
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("__obs_emit", |args, _out| {
        let event_value = args.first().cloned().unwrap_or(VmValue::Nil);
        let mut event = match vm_value_to_json(&event_value) {
            serde_json::Value::Object(map) => map,
            other => {
                return Err(VmError::Runtime(format!(
                    "__obs_emit: event must be a dict, got {}",
                    json_to_vm_value(&other).type_name()
                )));
            }
        };
        let explicit_backend = event.remove("backend");
        let emitted = emit_event(event, explicit_backend);
        Ok(json_to_vm_value(&emitted))
    });

    vm.register_builtin("__obs_events", |_args, _out| {
        let events = OBS_STATE.with(|state| state.borrow().emissions.clone());
        Ok(json_to_vm_value(&serde_json::Value::Array(events)))
    });

    vm.register_builtin("__obs_events_take", |_args, _out| {
        let events = OBS_STATE.with(|state| std::mem::take(&mut state.borrow_mut().emissions));
        Ok(json_to_vm_value(&serde_json::Value::Array(events)))
    });

    vm.register_builtin("__obs_auto_backend", |_args, _out| {
        Ok(json_to_vm_value(&resolve_auto_backend()))
    });
}

fn parse_config(value: &VmValue) -> Result<ObsConfig, VmError> {
    if matches!(value, VmValue::Nil) {
        return Ok(ObsConfig::default());
    }
    let serde_json::Value::Object(map) = vm_value_to_json(value) else {
        return Err(VmError::Runtime(
            "obs.configure: config must be a dict".to_string(),
        ));
    };
    let mut config = ObsConfig::default();
    if let Some(backend) = map.get("backend") {
        config.backend = backend.clone();
    }
    if let Some(serde_json::Value::Object(backends)) = map.get("backends") {
        config.backends = backends
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
    }
    if let Some(serde_json::Value::Array(routes)) = map.get("routes") {
        config.routes = routes.clone();
    }
    if let Some(serde_json::Value::Bool(enabled)) = map.get("audit_to_pretty_stderr") {
        config.audit_to_pretty_stderr = *enabled;
    }
    Ok(config)
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
        Some(VmValue::Dict(_)) => match vm_value_to_json(value.expect("checked above")) {
            serde_json::Value::Object(map) => Ok(map),
            _ => unreachable!(),
        },
        Some(other) => Err(VmError::Runtime(format!(
            "{builtin}: {field} must be a dict, got {}",
            other.type_name()
        ))),
    }
}

fn field_string(value: Option<&VmValue>, key: &str) -> Option<String> {
    let VmValue::Dict(map) = value? else {
        return None;
    };
    map.get(key).map(VmValue::display)
}

fn span_to_vm_value(span: &ObsSpan) -> VmValue {
    let mut out = BTreeMap::new();
    out.insert(
        "trace_id".to_string(),
        VmValue::String(Rc::from(span.trace_id.as_str())),
    );
    out.insert(
        "span_id".to_string(),
        VmValue::String(Rc::from(span.id.as_str())),
    );
    out.insert(
        "name".to_string(),
        VmValue::String(Rc::from(span.name.as_str())),
    );
    out.insert(
        "attrs".to_string(),
        json_to_vm_value(&serde_json::Value::Object(span.attrs.clone())),
    );
    VmValue::Dict(Rc::new(out))
}

fn base_event(
    kind: &str,
    name: &str,
    fields: Option<serde_json::Map<String, serde_json::Value>>,
    level: Option<String>,
    value: Option<serde_json::Value>,
) -> Result<serde_json::Map<String, serde_json::Value>, VmError> {
    let mut event = serde_json::Map::new();
    event.insert(
        "kind".to_string(),
        serde_json::Value::String(kind.to_string()),
    );
    event.insert(
        "name".to_string(),
        serde_json::Value::String(name.to_string()),
    );
    if let Some(fields) = fields {
        event.insert("fields".to_string(), serde_json::Value::Object(fields));
    }
    if let Some(level) = level {
        event.insert("level".to_string(), serde_json::Value::String(level));
    }
    if let Some(value) = value {
        event.insert("value".to_string(), value);
    }
    Ok(event)
}

fn emit_event(
    mut event: serde_json::Map<String, serde_json::Value>,
    explicit_backend: Option<serde_json::Value>,
) -> serde_json::Value {
    OBS_STATE.with(|state| {
        let mut state = state.borrow_mut();
        enrich_event(&mut event, state.span_stack.last());
        let event_value = serde_json::Value::Object(event.clone());
        let targets = if let Some(backend) = explicit_backend {
            vec![backend]
        } else {
            route_backends(&state.config, &event)
        };
        let mut delivered = Vec::new();
        for target in targets {
            match normalize_backend(&target) {
                Ok(backend) => delivered.extend(format_backend_payloads(&backend, &event_value)),
                Err(message) => {
                    delivered.push(audit_payload(&message, state.config.audit_to_pretty_stderr))
                }
            }
        }
        for payload in &delivered {
            maybe_write_pretty(payload);
        }
        state.emissions.extend(delivered.clone());
        serde_json::Value::Array(delivered)
    })
}

fn enrich_event(event: &mut serde_json::Map<String, serde_json::Value>, span: Option<&ObsSpan>) {
    event
        .entry("schema".to_string())
        .or_insert_with(|| serde_json::Value::String("harn.obs.event.v1".to_string()));
    if let Some(span) = span {
        event
            .entry("trace_id".to_string())
            .or_insert_with(|| serde_json::Value::String(span.trace_id.clone()));
        event
            .entry("span_id".to_string())
            .or_insert_with(|| serde_json::Value::String(span.id.clone()));
        let fields = event
            .entry("fields".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
        if let serde_json::Value::Object(fields) = fields {
            for (key, value) in &span.attrs {
                fields.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
    }
}

fn route_backends(
    config: &ObsConfig,
    event: &serde_json::Map<String, serde_json::Value>,
) -> Vec<serde_json::Value> {
    for route in &config.routes {
        if route_matches(route, event) {
            if let Some(target) = route
                .get("backend")
                .or_else(|| route.get("default"))
                .and_then(serde_json::Value::as_str)
            {
                return vec![resolve_named_backend(config, target)];
            }
        }
    }
    vec![config.backend.clone()]
}

fn route_matches(
    route: &serde_json::Value,
    event: &serde_json::Map<String, serde_json::Value>,
) -> bool {
    let serde_json::Value::Object(route) = route else {
        return false;
    };
    if route.get("default").is_some() {
        return true;
    }
    if let Some(expected) = route.get("kind").and_then(serde_json::Value::as_str) {
        if event.get("kind").and_then(serde_json::Value::as_str) == Some(expected) {
            return true;
        }
    }
    if let Some(expected) = route.get("level").and_then(serde_json::Value::as_str) {
        if event.get("level").and_then(serde_json::Value::as_str) == Some(expected) {
            return true;
        }
    }
    false
}

fn resolve_named_backend(config: &ObsConfig, target: &str) -> serde_json::Value {
    config
        .backends
        .get(target)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"kind": target, "id": target}))
}

fn normalize_backend(value: &serde_json::Value) -> Result<serde_json::Value, String> {
    let serde_json::Value::Object(map) = value else {
        return Err("backend must be a dict or named backend".to_string());
    };
    let kind = map
        .get("kind")
        .or_else(|| map.get("type"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match kind {
        "auto" => normalize_backend(&resolve_auto_backend()),
        "compose" => Ok(value.clone()),
        "otel" | "splunk_hec" | "honeycomb" | "pretty_stderr" | "test" => Ok(value.clone()),
        "" => Err("backend.kind is required".to_string()),
        other => Err(format!("unknown backend `{other}`")),
    }
}

fn resolve_auto_backend() -> serde_json::Value {
    if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        if !endpoint.trim().is_empty() {
            return serde_json::json!({"kind": "otel", "id": "otel", "endpoint": endpoint});
        }
    }
    if let Ok(endpoint) = std::env::var("HARN_OTEL_ENDPOINT") {
        if !endpoint.trim().is_empty() {
            return serde_json::json!({"kind": "otel", "id": "otel", "endpoint": endpoint});
        }
    }
    if let Ok(token) = std::env::var("SPLUNK_HEC_TOKEN") {
        if !token.trim().is_empty() {
            return serde_json::json!({"kind": "splunk_hec", "id": "splunk", "token": token});
        }
    }
    if let Ok(api_key) = std::env::var("HONEYCOMB_API_KEY") {
        if !api_key.trim().is_empty() {
            return serde_json::json!({"kind": "honeycomb", "id": "honeycomb", "api_key": api_key});
        }
    }
    serde_json::json!({"kind": "pretty_stderr", "id": "pretty_stderr"})
}

fn format_backend_payloads(
    backend: &serde_json::Value,
    event: &serde_json::Value,
) -> Vec<serde_json::Value> {
    let kind = backend
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if kind == "compose" {
        let payloads: Vec<_> = backend
            .get("backends")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .flat_map(|item| match normalize_backend(item) {
                        Ok(item) => format_backend_payloads(&item, event),
                        Err(message) => vec![audit_payload(&message, true)],
                    })
                    .collect()
            })
            .unwrap_or_default();
        return if payloads.is_empty() {
            vec![audit_payload("compose backend has no child backends", true)]
        } else {
            payloads
        };
    }
    let payload = match kind {
        "otel" => otel_payload(event),
        "splunk_hec" => splunk_payload(event),
        "honeycomb" => honeycomb_payload(event),
        "pretty_stderr" => pretty_payload(event),
        "test" => event.clone(),
        _ => audit_payload(&format!("unknown backend `{kind}`"), true),
    };
    vec![serde_json::json!({
        "backend": backend_label(backend),
        "format": kind,
        "payload": payload,
    })]
}

fn backend_label(backend: &serde_json::Value) -> serde_json::Value {
    backend
        .get("id")
        .or_else(|| backend.get("kind"))
        .cloned()
        .unwrap_or_else(|| serde_json::Value::String("unknown".to_string()))
}

fn otel_payload(event: &serde_json::Value) -> serde_json::Value {
    let kind = event
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match kind {
        "metric" => serde_json::json!({
            "resourceMetrics": [{
                "scopeMetrics": [{
                    "metrics": [{
                        "name": event.get("name").cloned().unwrap_or_default(),
                        "sum": {"dataPoints": [otel_data_point(event)]}
                    }]
                }]
            }]
        }),
        "span" | "span_end" => serde_json::json!({
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "name": event.get("name").cloned().unwrap_or_default(),
                        "traceId": event.get("trace_id").cloned().unwrap_or_default(),
                        "spanId": event.get("span_id").cloned().unwrap_or_default(),
                        "attributes": event.get("fields").cloned().unwrap_or_else(|| serde_json::json!({}))
                    }]
                }]
            }]
        }),
        _ => serde_json::json!({
            "resourceLogs": [{
                "scopeLogs": [{
                    "logRecords": [{
                        "body": event.get("message").or_else(|| event.get("name")).cloned().unwrap_or_default(),
                        "severityText": event.get("level").cloned().unwrap_or_else(|| serde_json::json!("info")),
                        "attributes": event.get("fields").cloned().unwrap_or_else(|| serde_json::json!({})),
                        "traceId": event.get("trace_id").cloned().unwrap_or_default(),
                        "spanId": event.get("span_id").cloned().unwrap_or_default()
                    }]
                }]
            }]
        }),
    }
}

fn otel_data_point(event: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "asDouble": event.get("value").cloned().unwrap_or_else(|| serde_json::json!(0)),
        "attributes": event.get("fields").cloned().unwrap_or_else(|| serde_json::json!({})),
    })
}

fn splunk_payload(event: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "sourcetype": "_json",
        "event": event,
    })
}

fn honeycomb_payload(event: &serde_json::Value) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    if let serde_json::Value::Object(map) = event {
        for (key, value) in map {
            if key != "fields" {
                out.insert(key.clone(), value.clone());
            }
        }
        if let Some(serde_json::Value::Object(fields)) = map.get("fields") {
            for (key, value) in fields {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    serde_json::Value::Object(out)
}

fn pretty_payload(event: &serde_json::Value) -> serde_json::Value {
    let level = event
        .get("level")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("info");
    let name = event
        .get("message")
        .or_else(|| event.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let mut fields = BTreeMap::new();
    if let Some(serde_json::Value::Object(map)) = event.get("fields") {
        for (key, value) in map {
            fields.insert(key.clone(), json_to_vm_value(value));
        }
    }
    serde_json::Value::String(
        vm_build_log_line(level, name, Some(&fields))
            .trim()
            .to_string(),
    )
}

fn audit_payload(message: &str, pretty_stderr: bool) -> serde_json::Value {
    serde_json::json!({
        "backend": if pretty_stderr { "pretty_stderr" } else { "audit" },
        "format": "audit",
        "payload": {
            "code": AUDIT_BACKEND_FAILURE,
            "message": message,
        }
    })
}

fn maybe_write_pretty(payload: &serde_json::Value) {
    let is_pretty =
        payload.get("format").and_then(serde_json::Value::as_str) == Some("pretty_stderr");
    if !is_pretty || VM_MIN_LOG_LEVEL.load(Ordering::Relaxed) > 1 {
        return;
    }
    if let Some(line) = payload.get("payload").and_then(serde_json::Value::as_str) {
        eprintln!("{line}");
    }
}

fn vm_value_to_json(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(value) => serde_json::json!(value),
        VmValue::Int(value) => serde_json::json!(value),
        VmValue::Float(value) => serde_json::json!(value),
        VmValue::String(value) => serde_json::json!(&**value),
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map.iter() {
                out.insert(key.clone(), vm_value_to_json(value));
            }
            serde_json::Value::Object(out)
        }
        VmValue::StructInstance { .. } => {
            let mut out = serde_json::Map::new();
            for (key, value) in value.struct_fields_map().unwrap_or_default().iter() {
                out.insert(key.clone(), vm_value_to_json(value));
            }
            serde_json::Value::Object(out)
        }
        _ => serde_json::json!(value.display()),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_obs_env(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().expect("observability env lock poisoned");
        let keys = [
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "HARN_OTEL_ENDPOINT",
            "SPLUNK_HEC_TOKEN",
            "HONEYCOMB_API_KEY",
        ];
        let saved: Vec<(&str, Option<String>)> = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        struct RestoreEnv(Vec<(&'static str, Option<String>)>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                for (key, value) in &self.0 {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
        let _restore = RestoreEnv(saved);
        for key in keys {
            std::env::remove_var(key);
        }
        for (key, value) in vars {
            if let Some(value) = value {
                std::env::set_var(key, value);
            }
        }
        f();
    }

    fn auto_kind() -> String {
        resolve_auto_backend()
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .expect("auto backend kind")
            .to_string()
    }

    #[test]
    fn auto_backend_prefers_otel_endpoint() {
        with_obs_env(
            &[("OTEL_EXPORTER_OTLP_ENDPOINT", Some("http://collector"))],
            || {
                assert_eq!(auto_kind(), "otel");
            },
        );
    }

    #[test]
    fn auto_backend_falls_through_to_splunk_then_honeycomb_then_pretty() {
        with_obs_env(&[("SPLUNK_HEC_TOKEN", Some("token"))], || {
            assert_eq!(auto_kind(), "splunk_hec");
        });
        with_obs_env(&[("HONEYCOMB_API_KEY", Some("key"))], || {
            assert_eq!(auto_kind(), "honeycomb");
        });
        with_obs_env(&[], || {
            assert_eq!(auto_kind(), "pretty_stderr");
        });
    }
}
