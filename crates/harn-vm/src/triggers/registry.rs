use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::event_log::{active_event_log, AnyEventLog, EventLog, LogEvent, Topic};
use crate::secrets::{configured_default_chain, SecretProvider};
use crate::value::VmClosure;

use super::ProviderId;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TriggerId(String);

impl TriggerId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TriggerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerState {
    Registering,
    Active,
    Draining,
    Terminated,
}

impl TriggerState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Registering => "registering",
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Terminated => "terminated",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerBindingSource {
    Manifest,
    Dynamic,
}

impl TriggerBindingSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Dynamic => "dynamic",
        }
    }
}

#[derive(Clone)]
pub enum TriggerHandlerSpec {
    Local { raw: String, closure: Rc<VmClosure> },
    A2a { target: String },
    Worker { queue: String },
}

impl std::fmt::Debug for TriggerHandlerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local { raw, .. } => f.debug_struct("Local").field("raw", raw).finish(),
            Self::A2a { target } => f.debug_struct("A2a").field("target", target).finish(),
            Self::Worker { queue } => f.debug_struct("Worker").field("queue", queue).finish(),
        }
    }
}

impl TriggerHandlerSpec {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Local { .. } => "local",
            Self::A2a { .. } => "a2a",
            Self::Worker { .. } => "worker",
        }
    }
}

#[derive(Clone)]
pub struct TriggerPredicateSpec {
    pub raw: String,
    pub closure: Rc<VmClosure>,
}

impl std::fmt::Debug for TriggerPredicateSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TriggerPredicateSpec")
            .field("raw", &self.raw)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct TriggerBindingSpec {
    pub id: String,
    pub source: TriggerBindingSource,
    pub kind: String,
    pub provider: ProviderId,
    pub handler: TriggerHandlerSpec,
    pub when: Option<TriggerPredicateSpec>,
    pub match_events: Vec<String>,
    pub dedupe_key: Option<String>,
    pub filter: Option<String>,
    pub daily_cost_usd: Option<f64>,
    pub max_concurrent: Option<u32>,
    pub manifest_path: Option<PathBuf>,
    pub package_name: Option<String>,
    pub definition_fingerprint: String,
}

#[derive(Debug)]
pub struct TriggerMetrics {
    pub received: AtomicU64,
    pub dispatched: AtomicU64,
    pub failed: AtomicU64,
    pub dlq: AtomicU64,
    pub last_received_ms: Mutex<Option<i64>>,
    pub cost_total_usd_micros: AtomicU64,
    pub cost_today_usd_micros: AtomicU64,
}

impl Default for TriggerMetrics {
    fn default() -> Self {
        Self {
            received: AtomicU64::new(0),
            dispatched: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            dlq: AtomicU64::new(0),
            last_received_ms: Mutex::new(None),
            cost_total_usd_micros: AtomicU64::new(0),
            cost_today_usd_micros: AtomicU64::new(0),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerMetricsSnapshot {
    pub received: u64,
    pub dispatched: u64,
    pub failed: u64,
    pub dlq: u64,
    pub in_flight: u64,
    pub last_received_ms: Option<i64>,
    pub cost_total_usd_micros: u64,
    pub cost_today_usd_micros: u64,
}

pub struct TriggerBinding {
    pub id: TriggerId,
    pub version: u32,
    pub source: TriggerBindingSource,
    pub kind: String,
    pub provider: ProviderId,
    pub handler: TriggerHandlerSpec,
    pub when: Option<TriggerPredicateSpec>,
    pub match_events: Vec<String>,
    pub dedupe_key: Option<String>,
    pub filter: Option<String>,
    pub daily_cost_usd: Option<f64>,
    pub max_concurrent: Option<u32>,
    pub manifest_path: Option<PathBuf>,
    pub package_name: Option<String>,
    pub definition_fingerprint: String,
    pub state: Mutex<TriggerState>,
    pub metrics: TriggerMetrics,
    pub in_flight: AtomicU64,
    pub cancel_token: Arc<AtomicBool>,
}

impl std::fmt::Debug for TriggerBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TriggerBinding")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("source", &self.source)
            .field("kind", &self.kind)
            .field("provider", &self.provider)
            .field("handler_kind", &self.handler.kind())
            .field("state", &self.state_snapshot())
            .finish()
    }
}

impl TriggerBinding {
    pub fn snapshot(&self) -> TriggerBindingSnapshot {
        TriggerBindingSnapshot {
            id: self.id.as_str().to_string(),
            version: self.version,
            source: self.source,
            kind: self.kind.clone(),
            provider: self.provider.as_str().to_string(),
            handler_kind: self.handler.kind().to_string(),
            state: self.state_snapshot(),
            metrics: self.metrics_snapshot(),
        }
    }

    fn new(spec: TriggerBindingSpec, version: u32) -> Self {
        Self {
            id: TriggerId::new(spec.id),
            version,
            source: spec.source,
            kind: spec.kind,
            provider: spec.provider,
            handler: spec.handler,
            when: spec.when,
            match_events: spec.match_events,
            dedupe_key: spec.dedupe_key,
            filter: spec.filter,
            daily_cost_usd: spec.daily_cost_usd,
            max_concurrent: spec.max_concurrent,
            manifest_path: spec.manifest_path,
            package_name: spec.package_name,
            definition_fingerprint: spec.definition_fingerprint,
            state: Mutex::new(TriggerState::Registering),
            metrics: TriggerMetrics::default(),
            in_flight: AtomicU64::new(0),
            cancel_token: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn binding_key(&self) -> String {
        format!("{}@v{}", self.id.as_str(), self.version)
    }

    pub fn state_snapshot(&self) -> TriggerState {
        *self.state.lock().expect("trigger state poisoned")
    }

    pub fn metrics_snapshot(&self) -> TriggerMetricsSnapshot {
        TriggerMetricsSnapshot {
            received: self.metrics.received.load(Ordering::Relaxed),
            dispatched: self.metrics.dispatched.load(Ordering::Relaxed),
            failed: self.metrics.failed.load(Ordering::Relaxed),
            dlq: self.metrics.dlq.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
            last_received_ms: *self
                .metrics
                .last_received_ms
                .lock()
                .expect("trigger metrics poisoned"),
            cost_total_usd_micros: self.metrics.cost_total_usd_micros.load(Ordering::Relaxed),
            cost_today_usd_micros: self.metrics.cost_today_usd_micros.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerBindingSnapshot {
    pub id: String,
    pub version: u32,
    pub source: TriggerBindingSource,
    pub kind: String,
    pub provider: String,
    pub handler_kind: String,
    pub state: TriggerState,
    pub metrics: TriggerMetricsSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerDispatchOutcome {
    Dispatched,
    Failed,
    Dlq,
}

#[derive(Debug)]
pub enum TriggerRegistryError {
    DuplicateId(String),
    InvalidSpec(String),
    UnknownId(String),
    UnknownBindingVersion { id: String, version: u32 },
    EventLog(String),
}

impl std::fmt::Display for TriggerRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "duplicate trigger id '{id}'"),
            Self::InvalidSpec(message) | Self::EventLog(message) => f.write_str(message),
            Self::UnknownId(id) => write!(f, "unknown trigger id '{id}'"),
            Self::UnknownBindingVersion { id, version } => {
                write!(f, "unknown trigger binding '{id}' version {version}")
            }
        }
    }
}

impl std::error::Error for TriggerRegistryError {}

#[derive(Default)]
pub struct TriggerRegistry {
    bindings: BTreeMap<String, Vec<Arc<TriggerBinding>>>,
    by_provider: BTreeMap<String, BTreeSet<String>>,
    event_log: Option<Arc<AnyEventLog>>,
    secret_provider: Option<Arc<dyn SecretProvider>>,
}

thread_local! {
    static TRIGGER_REGISTRY: RefCell<TriggerRegistry> = RefCell::new(TriggerRegistry::default());
}

pub fn clear_trigger_registry() {
    TRIGGER_REGISTRY.with(|slot| {
        *slot.borrow_mut() = TriggerRegistry::default();
    });
}

pub fn snapshot_trigger_bindings() -> Vec<TriggerBindingSnapshot> {
    TRIGGER_REGISTRY.with(|slot| {
        let registry = slot.borrow();
        let mut snapshots = Vec::new();
        for bindings in registry.bindings.values() {
            for binding in bindings {
                snapshots.push(binding.snapshot());
            }
        }
        snapshots.sort_by(|left, right| {
            left.id
                .cmp(&right.id)
                .then(left.version.cmp(&right.version))
                .then(left.state.as_str().cmp(right.state.as_str()))
        });
        snapshots
    })
}

#[allow(clippy::arc_with_non_send_sync)]
pub fn resolve_live_trigger_binding(
    id: &str,
    version: Option<u32>,
) -> Result<Arc<TriggerBinding>, TriggerRegistryError> {
    TRIGGER_REGISTRY.with(|slot| {
        let registry = slot.borrow();
        if let Some(version) = version {
            let binding = registry.binding(id, version).ok_or_else(|| {
                TriggerRegistryError::UnknownBindingVersion {
                    id: id.to_string(),
                    version,
                }
            })?;
            if binding.state_snapshot() == TriggerState::Terminated {
                return Err(TriggerRegistryError::UnknownBindingVersion {
                    id: id.to_string(),
                    version,
                });
            }
            return Ok(binding);
        }

        registry
            .live_bindings_any_source(id)
            .into_iter()
            .max_by_key(|binding| binding.version)
            .ok_or_else(|| TriggerRegistryError::UnknownId(id.to_string()))
    })
}

pub async fn install_manifest_triggers(
    specs: Vec<TriggerBindingSpec>,
) -> Result<(), TriggerRegistryError> {
    let (event_log, events) = TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        registry.refresh_runtime_context();

        let mut incoming = BTreeMap::new();
        for spec in specs {
            let spec_id = spec.id.clone();
            if spec.source != TriggerBindingSource::Manifest {
                return Err(TriggerRegistryError::InvalidSpec(format!(
                    "manifest install received non-manifest trigger '{}'",
                    spec_id
                )));
            }
            if spec_id.trim().is_empty() {
                return Err(TriggerRegistryError::InvalidSpec(
                    "manifest trigger id cannot be empty".to_string(),
                ));
            }
            if incoming.insert(spec_id.clone(), spec).is_some() {
                return Err(TriggerRegistryError::DuplicateId(spec_id));
            }
        }

        let mut lifecycle = Vec::new();
        let existing_ids: Vec<String> = registry
            .bindings
            .iter()
            .filter(|(_, bindings)| {
                bindings.iter().any(|binding| {
                    binding.source == TriggerBindingSource::Manifest
                        && binding.state_snapshot() != TriggerState::Terminated
                })
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in existing_ids {
            let live_manifest = registry.live_bindings(&id, TriggerBindingSource::Manifest);
            let Some(spec) = incoming.remove(&id) else {
                for binding in live_manifest {
                    registry.transition_binding_to_draining(&binding, &mut lifecycle);
                }
                continue;
            };

            let has_matching_active = live_manifest.iter().any(|binding| {
                binding.definition_fingerprint == spec.definition_fingerprint
                    && matches!(
                        binding.state_snapshot(),
                        TriggerState::Registering | TriggerState::Active
                    )
            });
            if has_matching_active {
                continue;
            }

            for binding in live_manifest {
                registry.transition_binding_to_draining(&binding, &mut lifecycle);
            }

            let version = registry.next_version_for_id(&id);
            registry.register_binding(spec, version, &mut lifecycle);
        }

        for spec in incoming.into_values() {
            let version = registry.next_version_for_id(&spec.id);
            registry.register_binding(spec, version, &mut lifecycle);
        }

        Ok((registry.event_log.clone(), lifecycle))
    })?;

    append_lifecycle_events(event_log, events).await
}

pub async fn dynamic_register(
    mut spec: TriggerBindingSpec,
) -> Result<TriggerId, TriggerRegistryError> {
    if spec.id.trim().is_empty() {
        spec.id = format!("dynamic_trigger_{}", Uuid::now_v7());
    }
    spec.source = TriggerBindingSource::Dynamic;
    let id = spec.id.clone();
    let (event_log, events) = TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        registry.refresh_runtime_context();

        if registry.bindings.contains_key(id.as_str()) {
            return Err(TriggerRegistryError::DuplicateId(id.clone()));
        }

        let mut lifecycle = Vec::new();
        registry.register_binding(spec, 1, &mut lifecycle);
        Ok((registry.event_log.clone(), lifecycle))
    })?;

    append_lifecycle_events(event_log, events).await?;
    Ok(TriggerId::new(id))
}

pub async fn dynamic_deregister(id: &str) -> Result<(), TriggerRegistryError> {
    let (event_log, events) = TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        let live_dynamic = registry.live_bindings(id, TriggerBindingSource::Dynamic);
        if live_dynamic.is_empty() {
            return Err(TriggerRegistryError::UnknownId(id.to_string()));
        }

        let mut lifecycle = Vec::new();
        for binding in live_dynamic {
            registry.transition_binding_to_draining(&binding, &mut lifecycle);
        }
        Ok((registry.event_log.clone(), lifecycle))
    })?;

    append_lifecycle_events(event_log, events).await
}

pub async fn drain(id: &str) -> Result<(), TriggerRegistryError> {
    let (event_log, events) = TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        let live = registry.live_bindings_any_source(id);
        if live.is_empty() {
            return Err(TriggerRegistryError::UnknownId(id.to_string()));
        }

        let mut lifecycle = Vec::new();
        for binding in live {
            registry.transition_binding_to_draining(&binding, &mut lifecycle);
        }
        Ok((registry.event_log.clone(), lifecycle))
    })?;

    append_lifecycle_events(event_log, events).await
}

pub fn begin_in_flight(id: &str, version: u32) -> Result<(), TriggerRegistryError> {
    TRIGGER_REGISTRY.with(|slot| {
        let registry = slot.borrow();
        let binding = registry.binding(id, version).ok_or_else(|| {
            TriggerRegistryError::UnknownBindingVersion {
                id: id.to_string(),
                version,
            }
        })?;
        match binding.state_snapshot() {
            TriggerState::Terminated => Err(TriggerRegistryError::InvalidSpec(format!(
                "trigger binding '{}' version {} is terminated",
                id, version
            ))),
            _ => {
                binding.in_flight.fetch_add(1, Ordering::Relaxed);
                binding.metrics.received.fetch_add(1, Ordering::Relaxed);
                *binding
                    .metrics
                    .last_received_ms
                    .lock()
                    .expect("trigger metrics poisoned") = Some(now_ms());
                Ok(())
            }
        }
    })
}

pub async fn finish_in_flight(
    id: &str,
    version: u32,
    outcome: TriggerDispatchOutcome,
) -> Result<(), TriggerRegistryError> {
    let (event_log, events) = TRIGGER_REGISTRY.with(|slot| {
        let registry = &mut *slot.borrow_mut();
        let binding = registry.binding(id, version).ok_or_else(|| {
            TriggerRegistryError::UnknownBindingVersion {
                id: id.to_string(),
                version,
            }
        })?;
        let current = binding.in_flight.load(Ordering::Relaxed);
        if current == 0 {
            return Err(TriggerRegistryError::InvalidSpec(format!(
                "trigger binding '{}' version {} has no in-flight events",
                id, version
            )));
        }
        binding.in_flight.fetch_sub(1, Ordering::Relaxed);
        match outcome {
            TriggerDispatchOutcome::Dispatched => {
                binding.metrics.dispatched.fetch_add(1, Ordering::Relaxed);
            }
            TriggerDispatchOutcome::Failed => {
                binding.metrics.failed.fetch_add(1, Ordering::Relaxed);
            }
            TriggerDispatchOutcome::Dlq => {
                binding.metrics.dlq.fetch_add(1, Ordering::Relaxed);
            }
        }

        let mut lifecycle = Vec::new();
        registry.maybe_finalize_draining(&binding, &mut lifecycle);
        Ok((registry.event_log.clone(), lifecycle))
    })?;

    append_lifecycle_events(event_log, events).await
}

impl TriggerRegistry {
    fn refresh_runtime_context(&mut self) {
        if self.event_log.is_none() {
            self.event_log = active_event_log();
        }
        if self.secret_provider.is_none() {
            self.secret_provider = default_secret_provider();
        }
    }

    fn binding(&self, id: &str, version: u32) -> Option<Arc<TriggerBinding>> {
        self.bindings
            .get(id)
            .and_then(|bindings| bindings.iter().find(|binding| binding.version == version))
            .cloned()
    }

    fn live_bindings(&self, id: &str, source: TriggerBindingSource) -> Vec<Arc<TriggerBinding>> {
        self.bindings
            .get(id)
            .into_iter()
            .flat_map(|bindings| bindings.iter())
            .filter(|binding| {
                binding.source == source && binding.state_snapshot() != TriggerState::Terminated
            })
            .cloned()
            .collect()
    }

    fn live_bindings_any_source(&self, id: &str) -> Vec<Arc<TriggerBinding>> {
        self.bindings
            .get(id)
            .into_iter()
            .flat_map(|bindings| bindings.iter())
            .filter(|binding| binding.state_snapshot() != TriggerState::Terminated)
            .cloned()
            .collect()
    }

    fn next_version_for_id(&self, id: &str) -> u32 {
        self.bindings
            .get(id)
            .into_iter()
            .flat_map(|bindings| bindings.iter())
            .map(|binding| binding.version)
            .max()
            .unwrap_or(0)
            + 1
    }

    #[allow(clippy::arc_with_non_send_sync)]
    fn register_binding(
        &mut self,
        spec: TriggerBindingSpec,
        version: u32,
        lifecycle: &mut Vec<LogEvent>,
    ) -> Arc<TriggerBinding> {
        let binding = Arc::new(TriggerBinding::new(spec, version));
        self.by_provider
            .entry(binding.provider.as_str().to_string())
            .or_default()
            .insert(binding.id.as_str().to_string());
        self.bindings
            .entry(binding.id.as_str().to_string())
            .or_default()
            .push(binding.clone());
        lifecycle.push(lifecycle_event(&binding, None, TriggerState::Registering));
        self.transition_binding_state(&binding, TriggerState::Active, lifecycle);
        binding
    }

    fn transition_binding_to_draining(
        &self,
        binding: &Arc<TriggerBinding>,
        lifecycle: &mut Vec<LogEvent>,
    ) {
        if matches!(binding.state_snapshot(), TriggerState::Terminated) {
            return;
        }
        self.transition_binding_state(binding, TriggerState::Draining, lifecycle);
        self.maybe_finalize_draining(binding, lifecycle);
    }

    fn maybe_finalize_draining(
        &self,
        binding: &Arc<TriggerBinding>,
        lifecycle: &mut Vec<LogEvent>,
    ) {
        if binding.state_snapshot() == TriggerState::Draining
            && binding.in_flight.load(Ordering::Relaxed) == 0
        {
            self.transition_binding_state(binding, TriggerState::Terminated, lifecycle);
        }
    }

    fn transition_binding_state(
        &self,
        binding: &Arc<TriggerBinding>,
        next: TriggerState,
        lifecycle: &mut Vec<LogEvent>,
    ) {
        let mut state = binding.state.lock().expect("trigger state poisoned");
        let previous = *state;
        if previous == next {
            return;
        }
        *state = next;
        drop(state);
        lifecycle.push(lifecycle_event(binding, Some(previous), next));
    }
}

fn lifecycle_event(
    binding: &TriggerBinding,
    from_state: Option<TriggerState>,
    to_state: TriggerState,
) -> LogEvent {
    LogEvent::new(
        "state_transition",
        serde_json::json!({
            "id": binding.id.as_str(),
            "binding_key": binding.binding_key(),
            "version": binding.version,
            "provider": binding.provider.as_str(),
            "kind": &binding.kind,
            "source": binding.source.as_str(),
            "handler_kind": binding.handler.kind(),
            "from_state": from_state.map(TriggerState::as_str),
            "to_state": to_state.as_str(),
        }),
    )
}

async fn append_lifecycle_events(
    event_log: Option<Arc<AnyEventLog>>,
    events: Vec<LogEvent>,
) -> Result<(), TriggerRegistryError> {
    let Some(event_log) = event_log else {
        return Ok(());
    };
    if events.is_empty() {
        return Ok(());
    }

    let topic = Topic::new("triggers.lifecycle")
        .expect("static triggers.lifecycle topic should always be valid");
    for event in events {
        event_log
            .append(&topic, event)
            .await
            .map_err(|error| TriggerRegistryError::EventLog(error.to_string()))?;
    }
    Ok(())
}

fn default_secret_provider() -> Option<Arc<dyn SecretProvider>> {
    configured_default_chain(default_secret_namespace())
        .ok()
        .map(|provider| Arc::new(provider) as Arc<dyn SecretProvider>)
}

fn default_secret_namespace() -> String {
    if let Ok(namespace) = std::env::var("HARN_SECRET_NAMESPACE") {
        if !namespace.trim().is_empty() {
            return namespace;
        }
    }

    let cwd = std::env::current_dir().unwrap_or_default();
    let leaf = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace");
    format!("harn/{leaf}")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{install_default_for_base_dir, reset_active_event_log};

    fn manifest_spec(id: &str, fingerprint: &str) -> TriggerBindingSpec {
        TriggerBindingSpec {
            id: id.to_string(),
            source: TriggerBindingSource::Manifest,
            kind: "webhook".to_string(),
            provider: ProviderId::from("github"),
            handler: TriggerHandlerSpec::Worker {
                queue: format!("{id}-queue"),
            },
            when: None,
            match_events: vec!["issues.opened".to_string()],
            dedupe_key: Some("event.dedupe_key".to_string()),
            filter: Some("event.kind".to_string()),
            daily_cost_usd: Some(5.0),
            max_concurrent: Some(10),
            manifest_path: None,
            package_name: Some("workspace".to_string()),
            definition_fingerprint: fingerprint.to_string(),
        }
    }

    fn dynamic_spec(id: &str) -> TriggerBindingSpec {
        TriggerBindingSpec {
            id: id.to_string(),
            source: TriggerBindingSource::Dynamic,
            kind: "webhook".to_string(),
            provider: ProviderId::from("github"),
            handler: TriggerHandlerSpec::Worker {
                queue: format!("{id}-queue"),
            },
            when: None,
            match_events: vec!["issues.opened".to_string()],
            dedupe_key: None,
            filter: None,
            daily_cost_usd: None,
            max_concurrent: None,
            manifest_path: None,
            package_name: None,
            definition_fingerprint: format!("dynamic:{id}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manifest_loaded_trigger_registers_with_zeroed_metrics() {
        clear_trigger_registry();

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
            .await
            .expect("manifest trigger installs");

        let snapshots = snapshot_trigger_bindings();
        assert_eq!(snapshots.len(), 1);
        let binding = &snapshots[0];
        assert_eq!(binding.id, "github-new-issue");
        assert_eq!(binding.version, 1);
        assert_eq!(binding.state, TriggerState::Active);
        assert_eq!(binding.metrics, TriggerMetricsSnapshot::default());

        clear_trigger_registry();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dynamic_register_assigns_unique_ids_and_rejects_duplicates() {
        clear_trigger_registry();

        let first = dynamic_register(dynamic_spec("dynamic-a"))
            .await
            .expect("first dynamic trigger");
        let second = dynamic_register(dynamic_spec("dynamic-b"))
            .await
            .expect("second dynamic trigger");
        assert_ne!(first, second);

        let error = dynamic_register(dynamic_spec("dynamic-a"))
            .await
            .expect_err("duplicate id should fail");
        assert!(matches!(error, TriggerRegistryError::DuplicateId(_)));

        clear_trigger_registry();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn drain_waits_for_in_flight_events_before_terminating() {
        clear_trigger_registry();

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
            .await
            .expect("manifest trigger installs");
        begin_in_flight("github-new-issue", 1).expect("start in-flight event");

        drain("github-new-issue").await.expect("drain succeeds");
        let binding = snapshot_trigger_bindings()
            .into_iter()
            .find(|binding| binding.id == "github-new-issue" && binding.version == 1)
            .expect("binding snapshot");
        assert_eq!(binding.state, TriggerState::Draining);
        assert_eq!(binding.metrics.in_flight, 1);

        finish_in_flight("github-new-issue", 1, TriggerDispatchOutcome::Dispatched)
            .await
            .expect("finish in-flight event");
        let binding = snapshot_trigger_bindings()
            .into_iter()
            .find(|binding| binding.id == "github-new-issue" && binding.version == 1)
            .expect("binding snapshot");
        assert_eq!(binding.state, TriggerState::Terminated);
        assert_eq!(binding.metrics.in_flight, 0);

        clear_trigger_registry();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hot_reload_registers_new_version_while_old_binding_drains() {
        clear_trigger_registry();

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
            .await
            .expect("initial manifest trigger installs");
        begin_in_flight("github-new-issue", 1).expect("start in-flight event");

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v2")])
            .await
            .expect("updated manifest trigger installs");

        let snapshots = snapshot_trigger_bindings();
        assert_eq!(snapshots.len(), 2);
        let old = snapshots
            .iter()
            .find(|binding| binding.id == "github-new-issue" && binding.version == 1)
            .expect("old binding");
        let new = snapshots
            .iter()
            .find(|binding| binding.id == "github-new-issue" && binding.version == 2)
            .expect("new binding");
        assert_eq!(old.state, TriggerState::Draining);
        assert_eq!(new.state, TriggerState::Active);

        finish_in_flight("github-new-issue", 1, TriggerDispatchOutcome::Dispatched)
            .await
            .expect("finish old in-flight event");
        let old = snapshot_trigger_bindings()
            .into_iter()
            .find(|binding| binding.id == "github-new-issue" && binding.version == 1)
            .expect("old binding");
        assert_eq!(old.state, TriggerState::Terminated);

        clear_trigger_registry();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lifecycle_transitions_append_to_event_log() {
        clear_trigger_registry();
        reset_active_event_log();
        let tempdir = tempfile::tempdir().expect("tempdir");
        let log = install_default_for_base_dir(tempdir.path()).expect("install event log");

        install_manifest_triggers(vec![manifest_spec("github-new-issue", "v1")])
            .await
            .expect("manifest trigger installs");
        begin_in_flight("github-new-issue", 1).expect("start in-flight event");
        drain("github-new-issue").await.expect("drain succeeds");
        finish_in_flight("github-new-issue", 1, TriggerDispatchOutcome::Dispatched)
            .await
            .expect("finish event");

        let topic = Topic::new("triggers.lifecycle").expect("valid lifecycle topic");
        let events = log
            .read_range(&topic, None, 32)
            .await
            .expect("read lifecycle events");
        let states: Vec<String> = events
            .into_iter()
            .filter_map(|(_, event)| {
                event
                    .payload
                    .get("to_state")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string())
            })
            .collect();
        assert_eq!(
            states,
            vec![
                "registering".to_string(),
                "active".to_string(),
                "draining".to_string(),
                "terminated".to_string(),
            ]
        );

        reset_active_event_log();
        clear_trigger_registry();
    }
}
