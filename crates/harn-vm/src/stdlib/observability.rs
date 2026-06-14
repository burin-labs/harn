use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
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
    processors: Vec<ObsProcessor>,
    audit_to_pretty_stderr: bool,
}

impl Default for ObsConfig {
    fn default() -> Self {
        Self {
            backend: serde_json::json!({"kind": "auto", "id": "auto"}),
            backends: std::collections::BTreeMap::new(),
            routes: Vec::new(),
            processors: Vec::new(),
            audit_to_pretty_stderr: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObsProcessor {
    Redaction,
}

impl ObsProcessor {
    fn parse(value: &serde_json::Value) -> Result<Self, VmError> {
        let kind = match value {
            serde_json::Value::String(kind) => kind.as_str(),
            serde_json::Value::Object(map) => map
                .get("kind")
                .or_else(|| map.get("type"))
                .or_else(|| map.get("name"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or(""),
            _ => {
                return Err(VmError::Runtime(
                    "obs.configure: processor must be a string or dict".to_string(),
                ))
            }
        };
        match kind {
            "redaction" | "redact" => Ok(Self::Redaction),
            "" => Err(VmError::Runtime(
                "obs.configure: processor.kind is required".to_string(),
            )),
            other => Err(VmError::Runtime(format!(
                "obs.configure: unknown processor `{other}`"
            ))),
        }
    }

    fn apply(self, event: &mut serde_json::Value) {
        match self {
            Self::Redaction => crate::redact::current_policy().redact_json_in_place(event),
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

/// Snapshot of the buffered emission payloads on the current thread.
/// Used by tests (e.g. the compass router) to assert that a primitive
/// emitted the metric it claims to. Mirrors the `__obs_events` builtin
/// without going through the Harn-script surface.
#[cfg(test)]
pub(crate) fn captured_emissions() -> Vec<serde_json::Value> {
    OBS_STATE.with(|state| state.borrow().emissions.clone())
}

/// Replace the active observability backend with a single named
/// backend. Callers pass values like `"pretty_stdout"`, `"pretty_stderr"`,
/// or `"otel"` — anything [`normalize_backend`] accepts. Errors when the
/// kind is unknown so the CLI can surface a typo before the server
/// boots.
///
/// This wires the same routing the `__obs_configure` builtin would
/// install, but without going through the Harn-script surface — used by
/// `harn serve --obs <MODE>` to seed routing before any handler runs.
pub fn install_default_backend(kind: &str) -> Result<(), String> {
    let backend = serde_json::json!({ "kind": kind, "id": kind });
    normalize_backend(&backend)?;
    OBS_STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.config.backend = backend;
    });
    Ok(())
}

pub(crate) fn register_observability_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(
    sig = "__obs_configure(...args: any) -> nil",
    category = "observability"
)]
fn obs_configure_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let config_value = args.first().cloned().unwrap_or(VmValue::Nil);
    let parsed = parse_config(&config_value)?;
    OBS_STATE.with(|state| state.borrow_mut().config = parsed);
    Ok(VmValue::Nil)
}

#[harn_builtin(sig = "__obs_reset(...args: any) -> nil", category = "observability")]
fn obs_reset_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    reset_observability_state();
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "__obs_start_span(...args: any) -> dict",
    category = "observability"
)]
fn obs_start_span_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
        let vm_span_id = crate::tracing::span_start(crate::tracing::SpanKind::FnCall, name.clone());
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
}

#[harn_builtin(
    sig = "__obs_end_span(...args: any) -> nil",
    category = "observability"
)]
fn obs_end_span_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "__obs_emit(...args: any) -> list", category = "observability")]
fn obs_emit_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "__obs_events(...args: any) -> list", category = "observability")]
fn obs_events_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let events = OBS_STATE.with(|state| state.borrow().emissions.clone());
    Ok(json_to_vm_value(&serde_json::Value::Array(events)))
}

#[harn_builtin(
    sig = "__obs_events_take(...args: any) -> list",
    category = "observability"
)]
fn obs_events_take_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let events = OBS_STATE.with(|state| std::mem::take(&mut state.borrow_mut().emissions));
    Ok(json_to_vm_value(&serde_json::Value::Array(events)))
}

#[harn_builtin(
    sig = "__obs_auto_backend(...args: any) -> dict",
    category = "observability"
)]
fn obs_auto_backend_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(json_to_vm_value(&resolve_auto_backend()))
}

/// Instrument kind for the standard metric primitives. Each variant
/// maps to the OTel instrument with matching semantics — counter is
/// monotonic-add, histogram records a distribution observation, gauge
/// sets a current value.
#[derive(Clone, Copy, Debug)]
pub(crate) enum MetricInstrument {
    Counter,
    Histogram,
    Gauge,
}

impl MetricInstrument {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            MetricInstrument::Counter => "counter",
            MetricInstrument::Histogram => "histogram",
            MetricInstrument::Gauge => "gauge",
        }
    }
}

#[harn_builtin(
    sig = "__obs_counter(...args: any) -> list",
    category = "observability"
)]
fn obs_counter_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    emit_instrument_from_args(MetricInstrument::Counter, args)
}

#[harn_builtin(
    sig = "__obs_histogram(...args: any) -> list",
    category = "observability"
)]
fn obs_histogram_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    emit_instrument_from_args(MetricInstrument::Histogram, args)
}

#[harn_builtin(sig = "__obs_gauge(...args: any) -> list", category = "observability")]
fn obs_gauge_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    emit_instrument_from_args(MetricInstrument::Gauge, args)
}

#[harn_builtin(
    sig = "__obs_request_id(...args: any) -> string|nil",
    category = "observability"
)]
fn obs_request_id_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(crate::observability::request_id::current_request_id()
        .map(|id| VmValue::String(std::sync::Arc::from(id.as_str())))
        .unwrap_or(VmValue::Nil))
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &OBS_CONFIGURE_IMPL_DEF,
    &OBS_RESET_IMPL_DEF,
    &OBS_START_SPAN_IMPL_DEF,
    &OBS_END_SPAN_IMPL_DEF,
    &OBS_EMIT_IMPL_DEF,
    &OBS_EVENTS_IMPL_DEF,
    &OBS_EVENTS_TAKE_IMPL_DEF,
    &OBS_AUTO_BACKEND_IMPL_DEF,
    &OBS_COUNTER_IMPL_DEF,
    &OBS_HISTOGRAM_IMPL_DEF,
    &OBS_GAUGE_IMPL_DEF,
    &OBS_REQUEST_ID_IMPL_DEF,
];

/// Common entry point for the counter/histogram/gauge builtins and for
/// the typed `harness.obs.{counter,histogram,gauge}` methods.
///
/// `attrs` keys are validated against
/// [`crate::observability::vocabulary`]; unknown keys under a known
/// `harn.<ns>.*` prefix raise a runtime audit error (`HARN-OBS-002`) so
/// typos surface at the call site, not at dashboard time. Keys outside
/// the `harn.*` vocabulary pass through unchanged (host- or
/// user-emitted business tags).
pub(crate) fn emit_instrument(
    instrument: MetricInstrument,
    name: String,
    value: serde_json::Value,
    attrs: serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value, VmError> {
    validate_vocabulary(&attrs, instrument.as_str(), &name)?;
    let mut event = base_event("metric", &name, Some(attrs), None, Some(value))?;
    event.insert(
        "instrument".to_string(),
        serde_json::Value::String(instrument.as_str().to_string()),
    );
    Ok(emit_event(event, None))
}

/// Typed start_span helper backing both `__obs_start_span` and
/// `harness.obs.start_span`. Returns the span dict as a `VmValue`.
pub(crate) fn start_span_typed(
    name: String,
    attrs: serde_json::Map<String, serde_json::Value>,
) -> Result<VmValue, VmError> {
    validate_vocabulary(&attrs, "span", &name)?;
    let args = vec![
        VmValue::String(std::sync::Arc::from(name.as_str())),
        json_to_vm_value(&serde_json::Value::Object(attrs)),
    ];
    obs_start_span_impl(&args, &mut String::new())
}

/// Typed end_span helper. `span_handle` must be the dict returned by
/// [`start_span_typed`]. Closing the innermost span by passing `nil` is
/// the documented imperative-style fallback.
pub(crate) fn end_span_typed(span_handle: VmValue) {
    let args = [span_handle];
    let _ = obs_end_span_impl(&args, &mut String::new());
}

/// Typed log helper backing `harness.obs.log`. `level` defaults to
/// `"info"` when empty.
pub(crate) fn log_typed(
    message: String,
    level: String,
    fields: serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value, VmError> {
    validate_vocabulary(&fields, "log", &message)?;
    let level = if level.is_empty() {
        "info".to_string()
    } else {
        level
    };
    let event = base_event("log", &message, Some(fields), Some(level), None)?;
    Ok(emit_event(event, None))
}

fn emit_instrument_from_args(
    instrument: MetricInstrument,
    args: &[VmValue],
) -> Result<VmValue, VmError> {
    let name = string_arg(args.first(), "__obs_metric", "name")?;
    let value_arg = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let value_json = vm_value_to_json(&value_arg);
    if !matches!(
        value_json,
        serde_json::Value::Number(_) | serde_json::Value::Null
    ) {
        return Err(VmError::Runtime(format!(
            "__obs_{}: value must be a number, got {}",
            instrument.as_str(),
            value_arg.type_name()
        )));
    }
    let attrs = object_arg(args.get(2), "__obs_metric", "attrs")?;
    let emitted = emit_instrument(instrument, name, value_json, attrs)?;
    Ok(json_to_vm_value(&emitted))
}

/// Validate attribute keys against the standardised vocabulary
/// (`crate::observability::vocabulary`). Unknown keys under a known
/// `harn.<ns>.*` prefix raise `HARN-OBS-002` so primitives can't drift
/// from the published schema undetected.
fn validate_vocabulary(
    attrs: &serde_json::Map<String, serde_json::Value>,
    surface: &str,
    name: &str,
) -> Result<(), VmError> {
    for key in attrs.keys() {
        if crate::observability::vocabulary::is_violation(key) {
            return Err(VmError::Runtime(format!(
                "HARN-OBS-002: {surface} `{name}` attribute `{key}` is not declared in the harn.* observability vocabulary"
            )));
        }
    }
    Ok(())
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
    if let Some(processors) = map.get("processors") {
        let serde_json::Value::Array(processors) = processors else {
            return Err(VmError::Runtime(
                "obs.configure: processors must be a list".to_string(),
            ));
        };
        config.processors = processors
            .iter()
            .map(ObsProcessor::parse)
            .collect::<Result<_, _>>()?;
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
    let mut out = crate::value::DictMap::new();
    out.put_str("trace_id", span.trace_id.as_str());
    out.put_str("span_id", span.id.as_str());
    out.put_str("name", span.name.as_str());
    out.insert(
        "attrs".to_string(),
        json_to_vm_value(&serde_json::Value::Object(span.attrs.clone())),
    );
    VmValue::dict(out)
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
        apply_processors(&state.config.processors, &mut event);
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
                    delivered.push(audit_payload(&message, state.config.audit_to_pretty_stderr));
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

fn apply_processors(
    processors: &[ObsProcessor],
    event: &mut serde_json::Map<String, serde_json::Value>,
) {
    if processors.is_empty() {
        return;
    }
    let mut event_value = serde_json::Value::Object(std::mem::take(event));
    for processor in processors {
        processor.apply(&mut event_value);
    }
    let serde_json::Value::Object(processed) = event_value else {
        return;
    };
    *event = processed;
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
    // Cross-cutting attributes from the dispatching host's ambient
    // scopes — populated once on every event so primitive emit sites
    // don't have to thread them through method signatures. Stored as
    // top-level keys (not under `fields`) so backends with native
    // request/tenant indexing surface them without descending.
    if !event.contains_key("request_id") {
        if let Some(id) = crate::observability::request_id::current_request_id() {
            event.insert("request_id".to_string(), serde_json::Value::String(id));
        }
    }
    if !event.contains_key("tenant_id") {
        if let Some(tenant) = crate::harness_tenant::current_tenant_id() {
            event.insert("tenant_id".to_string(), serde_json::Value::String(tenant.0));
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
        "otel" | "splunk_hec" | "honeycomb" | "pretty_stderr" | "pretty_stdout" | "test" => {
            Ok(value.clone())
        }
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
        "pretty_stderr" | "pretty_stdout" => pretty_payload(event),
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
    let mut fields = crate::value::DictMap::new();
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
    let format = payload
        .get("format")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let to_stdout = match format {
        "pretty_stderr" => false,
        "pretty_stdout" => true,
        _ => return,
    };
    if VM_MIN_LOG_LEVEL.load(Ordering::Relaxed) > 1 {
        return;
    }
    if let Some(line) = payload.get("payload").and_then(serde_json::Value::as_str) {
        if to_stdout {
            println!("{line}");
        } else {
            eprintln!("{line}");
        }
    }
}

pub(crate) fn vm_value_to_json(value: &VmValue) -> serde_json::Value {
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(value) => serde_json::json!(value),
        VmValue::Int(value) => serde_json::json!(value),
        VmValue::Float(value) => serde_json::json!(value),
        // Decimal serializes as a string to preserve exact precision.
        VmValue::Decimal(d) => serde_json::json!(d.to_string()),
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

    #[test]
    fn parses_stock_processors() {
        let config = parse_config(&json_to_vm_value(&serde_json::json!({
            "processors": ["redaction", {"kind": "redaction"}],
        })))
        .expect("config parses");
        assert_eq!(
            config.processors,
            vec![ObsProcessor::Redaction, ObsProcessor::Redaction]
        );
    }

    #[test]
    fn rejects_unknown_processors() {
        let error = parse_config(&json_to_vm_value(&serde_json::json!({
            "processors": ["forwarder"],
        })))
        .expect_err("unknown processor is rejected");
        assert!(
            error
                .to_string()
                .contains("obs.configure: unknown processor `forwarder`"),
            "{error}"
        );
    }

    #[test]
    fn redaction_processor_scrubs_events_before_backend_formatting() {
        reset_observability_state();
        let _ = obs_configure_impl(
            &[json_to_vm_value(&serde_json::json!({
                "backend": {"kind": "test", "id": "test"},
                "processors": ["redaction"],
            }))],
            &mut String::new(),
        )
        .expect("configure observability");

        let mut fields = serde_json::Map::new();
        fields.insert(
            "api_key".to_string(),
            serde_json::Value::String("sk_live_1234567890abcdef".to_string()),
        );
        fields.insert(
            "nested".to_string(),
            serde_json::json!({"access_token": "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}),
        );
        fields.insert(
            "safe".to_string(),
            serde_json::Value::String("kept".to_string()),
        );
        let mut event = base_event(
            "log",
            "redaction_smoke",
            Some(fields),
            Some("info".to_string()),
            None,
        )
        .expect("base event");
        event.insert(
            "message".to_string(),
            serde_json::Value::String(
                "https://user:pass@example.test/hook?token=secret".to_string(),
            ),
        );

        let emitted = emit_event(event, None);
        let payload = &emitted
            .as_array()
            .expect("emitted list")
            .first()
            .expect("first emission")["payload"];
        assert_eq!(payload["fields"]["api_key"], "[redacted]");
        assert_eq!(payload["fields"]["nested"]["access_token"], "[redacted]");
        assert_eq!(payload["fields"]["safe"], "kept");
        let rendered = payload.to_string();
        assert!(!rendered.contains("sk_live_1234567890abcdef"));
        assert!(!rendered.contains("ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(!rendered.contains("token=secret"));
        reset_observability_state();
    }
}
