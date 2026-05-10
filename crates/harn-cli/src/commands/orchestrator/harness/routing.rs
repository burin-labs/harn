//! Trigger routing, connector initialization, and the placeholder connector.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use harn_vm::clock::Clock;
use serde::Serialize;
use serde_json::Value as JsonValue;

use super::super::errors::OrchestratorError;
use super::super::listener::{RouteConfig, TriggerMetricSnapshot};
use crate::package::{
    self, CollectedManifestTrigger, CollectedTriggerHandler, ResolvedProviderConnectorConfig,
    ResolvedProviderConnectorKind, ResolvedTriggerConfig,
};

use super::audit::TriggerStateSnapshot;

pub(super) struct ConnectorRuntime {
    pub(super) registry: harn_vm::ConnectorRegistry,
    pub(super) trigger_registry: harn_vm::TriggerRegistry,
    pub(super) handles: Vec<harn_vm::connectors::ConnectorHandle>,
    pub(super) providers: Vec<String>,
    pub(super) activations: Vec<harn_vm::ActivationHandle>,
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(super) provider_overrides: Vec<ResolvedProviderConnectorConfig>,
}

#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct ManifestReloadSummary {
    pub(super) added: Vec<String>,
    pub(super) modified: Vec<String>,
    pub(super) removed: Vec<String>,
    pub(super) unchanged: Vec<String>,
}

#[cfg_attr(not(unix), allow(dead_code))]
pub(super) fn summarize_manifest_reload(
    current: &[CollectedManifestTrigger],
    next: &[CollectedManifestTrigger],
) -> ManifestReloadSummary {
    let current_map = trigger_fingerprint_map(current, true);
    let next_map = trigger_fingerprint_map(next, true);
    let mut summary = ManifestReloadSummary::default();
    let ids: BTreeSet<String> = current_map.keys().chain(next_map.keys()).cloned().collect();
    for id in ids {
        match (current_map.get(&id), next_map.get(&id)) {
            (None, Some(_)) => summary.added.push(id),
            (Some(_), None) => summary.removed.push(id),
            (Some(left), Some(right)) if left == right => summary.unchanged.push(id),
            (Some(_), Some(_)) => summary.modified.push(id),
            (None, None) => {}
        }
    }
    summary
}

#[cfg_attr(not(unix), allow(dead_code))]
pub(super) fn trigger_fingerprint_map(
    triggers: &[CollectedManifestTrigger],
    include_http_managed: bool,
) -> BTreeMap<String, String> {
    triggers
        .iter()
        .filter(|trigger| include_http_managed || !is_http_managed_trigger(trigger))
        .map(|trigger| {
            let spec = package::manifest_trigger_binding_spec(trigger.clone());
            (trigger.config.id.clone(), spec.definition_fingerprint)
        })
        .collect()
}

#[cfg_attr(not(unix), allow(dead_code))]
pub(super) fn connector_reload_fingerprint_map(
    triggers: &[CollectedManifestTrigger],
    provider_overrides: &[ResolvedProviderConnectorConfig],
) -> BTreeMap<String, Vec<String>> {
    let mut by_provider = BTreeMap::<String, Vec<String>>::new();
    for trigger in triggers {
        let provider = trigger.config.provider.as_str().to_string();
        if !connector_owns_ingress(&provider, provider_overrides)
            && matches!(
                trigger.config.kind,
                crate::package::TriggerKind::Webhook | crate::package::TriggerKind::A2aPush
            )
        {
            continue;
        }
        let spec = package::manifest_trigger_binding_spec(trigger.clone());
        by_provider
            .entry(provider)
            .or_default()
            .push(spec.definition_fingerprint);
    }
    for override_config in provider_overrides {
        by_provider
            .entry(override_config.id.as_str().to_string())
            .or_default()
            .push(provider_connector_fingerprint(override_config));
    }
    for fingerprints in by_provider.values_mut() {
        fingerprints.sort();
    }
    by_provider
}

#[cfg_attr(not(unix), allow(dead_code))]
fn provider_connector_fingerprint(config: &ResolvedProviderConnectorConfig) -> String {
    match &config.connector {
        ResolvedProviderConnectorKind::RustBuiltin => format!(
            "{}::builtin@{}",
            config.id.as_str(),
            config.manifest_dir.display()
        ),
        ResolvedProviderConnectorKind::Harn { module } => format!(
            "{}::harn:{}@{}",
            config.id.as_str(),
            module,
            config.manifest_dir.display()
        ),
        ResolvedProviderConnectorKind::Invalid(message) => format!(
            "{}::invalid:{}@{}",
            config.id.as_str(),
            message,
            config.manifest_dir.display()
        ),
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
fn is_http_managed_trigger(trigger: &CollectedManifestTrigger) -> bool {
    matches!(
        trigger.config.kind,
        crate::package::TriggerKind::Webhook | crate::package::TriggerKind::A2aPush
    )
}

// ── Connector helpers ─────────────────────────────────────────────────────────

pub(super) async fn initialize_connectors(
    triggers: &[CollectedManifestTrigger],
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    secrets: Arc<dyn harn_vm::secrets::SecretProvider>,
    metrics: Arc<harn_vm::MetricsRegistry>,
    provider_overrides: &[ResolvedProviderConnectorConfig],
    clock: Arc<dyn Clock>,
) -> Result<ConnectorRuntime, OrchestratorError> {
    let mut registry = harn_vm::ConnectorRegistry::default();
    let mut trigger_registry = harn_vm::TriggerRegistry::default();
    let mut grouped_kinds: BTreeMap<harn_vm::ProviderId, BTreeSet<String>> = BTreeMap::new();

    for trigger in triggers {
        let binding = trigger_binding_for(&trigger.config)?;
        grouped_kinds
            .entry(binding.provider.clone())
            .or_default()
            .insert(binding.kind.as_str().to_string());
        trigger_registry.register(binding);
    }

    let ctx = harn_vm::ConnectorCtx {
        inbox: Arc::new(
            harn_vm::InboxIndex::new(event_log.clone(), metrics.clone())
                .await
                .map_err(|error| error.to_string())?,
        ),
        event_log,
        secrets,
        metrics,
        rate_limiter: Arc::new(harn_vm::RateLimiterFactory::default()),
    };

    let mut providers = Vec::new();
    let mut handles = Vec::new();
    for (provider, kinds) in grouped_kinds {
        let provider_name = provider.as_str().to_string();
        if let Some(connector) = connector_override_for(&provider, provider_overrides).await? {
            registry.remove(&provider);
            registry
                .register(connector)
                .map_err(|error| error.to_string())?;
        }
        if registry.get(&provider).is_none() {
            if provider_requires_harn_connector(provider.as_str()) {
                return Err(format!(
                    "provider '{}' is package-backed; add [[providers]] id = \"{}\" with \
                     connector = {{ harn = \"...\" }} to the manifest",
                    provider.as_str(),
                    provider.as_str()
                )
                .into());
            }
            let connector = connector_for(&provider, kinds, clock.clone());
            registry
                .register(connector)
                .map_err(|error| error.to_string())?;
        }
        let handle = registry
            .get(&provider)
            .ok_or_else(|| format!("connector registry lost provider '{}'", provider.as_str()))?;
        handle
            .lock()
            .await
            .init(ctx.clone())
            .await
            .map_err(|error| error.to_string())?;
        handles.push(handle.clone());
        providers.push(provider_name);
    }

    Ok(ConnectorRuntime {
        registry,
        trigger_registry,
        handles,
        providers,
        activations: Vec::new(),
        provider_overrides: provider_overrides.to_vec(),
    })
}

fn trigger_binding_for(
    config: &ResolvedTriggerConfig,
) -> Result<harn_vm::TriggerBinding, OrchestratorError> {
    Ok(harn_vm::TriggerBinding {
        provider: config.provider.clone(),
        kind: harn_vm::TriggerKind::from(trigger_kind_name(config.kind)),
        binding_id: config.id.clone(),
        dedupe_key: config.dedupe_key.clone(),
        dedupe_retention_days: config.retry.retention_days,
        config: connector_binding_config(config)?,
    })
}

fn connector_binding_config(
    config: &ResolvedTriggerConfig,
) -> Result<JsonValue, OrchestratorError> {
    match config.kind {
        crate::package::TriggerKind::Cron => {
            serde_json::to_value(&config.kind_specific).map_err(|error| {
                OrchestratorError::Serve({
                    format!(
                        "failed to encode cron trigger config '{}': {error}",
                        config.id
                    )
                })
            })
        }
        crate::package::TriggerKind::Webhook => Ok(serde_json::json!({
            "match": config.match_,
            "secrets": config.secrets,
            "webhook": config.kind_specific,
        })),
        crate::package::TriggerKind::Poll => Ok(serde_json::json!({
            "match": config.match_,
            "secrets": config.secrets,
            "poll": config.kind_specific,
        })),
        crate::package::TriggerKind::Stream => Ok(serde_json::json!({
            "match": config.match_,
            "secrets": config.secrets,
            "stream": config.kind_specific,
            "window": config.window,
        })),
        crate::package::TriggerKind::A2aPush => Ok(serde_json::json!({
            "match": config.match_,
            "secrets": config.secrets,
            "a2a_push": a2a_push_connector_config(&config.kind_specific)?,
        })),
        _ => Ok(JsonValue::Null),
    }
}

fn a2a_push_connector_config(
    kind_specific: &BTreeMap<String, toml::Value>,
) -> Result<JsonValue, OrchestratorError> {
    if let Some(nested) = kind_specific.get("a2a_push") {
        return serde_json::to_value(nested).map_err(|error| {
            OrchestratorError::Serve(format!("failed to encode a2a_push trigger config: {error}"))
        });
    }
    let filtered = kind_specific
        .iter()
        .filter(|(key, _)| key.as_str() != "path")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    serde_json::to_value(filtered).map_err(|error| {
        OrchestratorError::Serve(format!("failed to encode a2a_push trigger config: {error}"))
    })
}

fn connector_for(
    provider: &harn_vm::ProviderId,
    kinds: BTreeSet<String>,
    clock: Arc<dyn Clock>,
) -> Box<dyn harn_vm::Connector> {
    match provider.as_str() {
        "cron" => Box::new(harn_vm::CronConnector::with_clock(clock)),
        _ => Box::new(PlaceholderConnector::new(provider.clone(), kinds)),
    }
}

async fn connector_override_for(
    provider: &harn_vm::ProviderId,
    provider_overrides: &[ResolvedProviderConnectorConfig],
) -> Result<Option<Box<dyn harn_vm::Connector>>, OrchestratorError> {
    let Some(override_config) = provider_overrides
        .iter()
        .find(|entry| entry.id == *provider)
    else {
        return Ok(None);
    };
    match &override_config.connector {
        ResolvedProviderConnectorKind::RustBuiltin => Ok(None),
        ResolvedProviderConnectorKind::Invalid(message) => {
            Err(OrchestratorError::Serve(message.clone()))
        }
        ResolvedProviderConnectorKind::Harn { module } => {
            let module_path =
                harn_vm::resolve_module_import_path(&override_config.manifest_dir, module);
            let connector = harn_vm::HarnConnector::load(&module_path)
                .await
                .map_err(|error| {
                    format!(
                        "failed to load Harn connector '{}' for provider '{}': {error}",
                        module_path.display(),
                        provider.as_str()
                    )
                })?;
            Ok(Some(Box::new(connector)))
        }
    }
}

pub(super) fn build_route_configs(
    triggers: &[CollectedManifestTrigger],
    binding_versions: &BTreeMap<String, u32>,
) -> Result<Vec<RouteConfig>, OrchestratorError> {
    let mut seen_paths = BTreeSet::new();
    let mut routes = Vec::new();
    for trigger in triggers {
        let Some(binding_version) = binding_versions.get(&trigger.config.id).copied() else {
            return Err(format!(
                "trigger registry is missing active manifest binding '{}'",
                trigger.config.id
            )
            .into());
        };
        if let Some(route) = RouteConfig::from_trigger(trigger, binding_version)? {
            if !seen_paths.insert(route.path.clone()) {
                return Err(format!(
                    "trigger route '{}' is configured more than once",
                    route.path
                )
                .into());
            }
            routes.push(route);
        }
    }
    Ok(routes)
}

pub(super) fn attach_route_connectors(
    routes: Vec<RouteConfig>,
    registry: &harn_vm::ConnectorRegistry,
    provider_overrides: &[ResolvedProviderConnectorConfig],
) -> Result<Vec<RouteConfig>, OrchestratorError> {
    routes
        .into_iter()
        .map(|mut route| {
            if route.connector_ingress
                || connector_owns_ingress(route.provider.as_str(), provider_overrides)
            {
                route.connector = Some(registry.get(&route.provider).ok_or_else(|| {
                    format!(
                        "connector registry is missing provider '{}'",
                        route.provider.as_str()
                    )
                })?);
            }
            Ok(route)
        })
        .collect()
}

pub(super) fn connector_owns_ingress(
    provider: &str,
    provider_overrides: &[ResolvedProviderConnectorConfig],
) -> bool {
    provider_overrides.iter().any(|entry| {
        entry.id.as_str() == provider
            && matches!(entry.connector, ResolvedProviderConnectorKind::Harn { .. })
    })
}

fn provider_requires_harn_connector(provider: &str) -> bool {
    harn_vm::provider_metadata(provider).is_some_and(|metadata| {
        matches!(
            metadata.runtime,
            harn_vm::ProviderRuntimeMetadata::Placeholder
        )
    })
}

pub(super) fn live_manifest_binding_versions() -> BTreeMap<String, u32> {
    let mut versions = BTreeMap::new();
    for binding in harn_vm::snapshot_trigger_bindings() {
        if binding.source != harn_vm::TriggerBindingSource::Manifest {
            continue;
        }
        if binding.state == harn_vm::TriggerState::Terminated {
            continue;
        }
        versions
            .entry(binding.id)
            .and_modify(|current: &mut u32| *current = (*current).max(binding.version))
            .or_insert(binding.version);
    }
    versions
}

pub(super) fn trigger_state_snapshots(
    triggers: &[CollectedManifestTrigger],
    listener_metrics: &BTreeMap<String, TriggerMetricSnapshot>,
) -> Vec<TriggerStateSnapshot> {
    let bindings_by_id = harn_vm::snapshot_trigger_bindings()
        .into_iter()
        .filter(|binding| binding.source == harn_vm::TriggerBindingSource::Manifest)
        .fold(
            BTreeMap::<String, harn_vm::TriggerBindingSnapshot>::new(),
            |mut acc, binding| {
                match acc.get(&binding.id) {
                    Some(current) if current.version >= binding.version => {}
                    _ => {
                        acc.insert(binding.id.clone(), binding);
                    }
                }
                acc
            },
        );

    triggers
        .iter()
        .map(|trigger| {
            let runtime = bindings_by_id.get(&trigger.config.id);
            let metrics = listener_metrics.get(&trigger.config.id);
            TriggerStateSnapshot {
                id: trigger.config.id.clone(),
                provider: trigger.config.provider.as_str().to_string(),
                kind: trigger_kind_name(trigger.config.kind).to_string(),
                handler: handler_kind(&trigger.handler).to_string(),
                version: runtime.map(|binding| binding.version),
                state: runtime.map(|binding| binding.state.as_str().to_string()),
                received: metrics.map(|value| value.received).unwrap_or(0),
                dispatched: metrics.map(|value| value.dispatched).unwrap_or(0),
                failed: metrics.map(|value| value.failed).unwrap_or(0),
                in_flight: metrics.map(|value| value.in_flight).unwrap_or(0),
            }
        })
        .collect()
}

pub(super) fn format_trigger_summary(triggers: &[CollectedManifestTrigger]) -> String {
    if triggers.is_empty() {
        return "none".to_string();
    }
    triggers
        .iter()
        .map(|trigger| {
            format!(
                "{} [{}:{} -> {}]",
                trigger.config.id,
                trigger.config.provider.as_str(),
                trigger_kind_name(trigger.config.kind),
                handler_kind(&trigger.handler)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn format_activation_summary(activations: &[harn_vm::ActivationHandle]) -> String {
    if activations.is_empty() {
        return "none".to_string();
    }
    activations
        .iter()
        .map(|activation| {
            format!(
                "{}({})",
                activation.provider.as_str(),
                activation.binding_count
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn handler_kind(handler: &CollectedTriggerHandler) -> &'static str {
    match handler {
        CollectedTriggerHandler::Local { .. } => "local",
        CollectedTriggerHandler::A2a { .. } => "a2a",
        CollectedTriggerHandler::Worker { .. } => "worker",
        CollectedTriggerHandler::Persona { .. } => "persona",
    }
}

pub(super) fn trigger_kind_name(kind: crate::package::TriggerKind) -> &'static str {
    match kind {
        crate::package::TriggerKind::Webhook => "webhook",
        crate::package::TriggerKind::Cron => "cron",
        crate::package::TriggerKind::Poll => "poll",
        crate::package::TriggerKind::Stream => "stream",
        crate::package::TriggerKind::Predicate => "predicate",
        crate::package::TriggerKind::A2aPush => "a2a-push",
    }
}

// ── Placeholder connector ─────────────────────────────────────────────────────

struct PlaceholderConnector {
    provider_id: harn_vm::ProviderId,
    kinds: Vec<harn_vm::TriggerKind>,
    _ctx: Option<harn_vm::ConnectorCtx>,
}

impl PlaceholderConnector {
    fn new(provider_id: harn_vm::ProviderId, kinds: BTreeSet<String>) -> Self {
        Self {
            provider_id,
            kinds: kinds.into_iter().map(harn_vm::TriggerKind::from).collect(),
            _ctx: None,
        }
    }
}

struct PlaceholderClient;

#[async_trait]
impl harn_vm::ConnectorClient for PlaceholderClient {
    async fn call(
        &self,
        method: &str,
        _args: JsonValue,
    ) -> Result<JsonValue, harn_vm::ClientError> {
        Err(harn_vm::ClientError::Other(format!(
            "connector client method '{method}' is not implemented in the orchestrator scaffold"
        )))
    }
}

#[async_trait]
impl harn_vm::Connector for PlaceholderConnector {
    fn provider_id(&self) -> &harn_vm::ProviderId {
        &self.provider_id
    }

    fn kinds(&self) -> &[harn_vm::TriggerKind] {
        &self.kinds
    }

    async fn init(&mut self, ctx: harn_vm::ConnectorCtx) -> Result<(), harn_vm::ConnectorError> {
        self._ctx = Some(ctx);
        Ok(())
    }

    async fn activate(
        &self,
        bindings: &[harn_vm::TriggerBinding],
    ) -> Result<harn_vm::ActivationHandle, harn_vm::ConnectorError> {
        Ok(harn_vm::ActivationHandle::new(
            self.provider_id.clone(),
            bindings.len(),
        ))
    }

    async fn normalize_inbound(
        &self,
        _raw: harn_vm::RawInbound,
    ) -> Result<harn_vm::TriggerEvent, harn_vm::ConnectorError> {
        Err(harn_vm::ConnectorError::Unsupported(format!(
            "connector '{}' inbound normalization is not implemented yet",
            self.provider_id.as_str()
        )))
    }

    fn payload_schema(&self) -> harn_vm::ProviderPayloadSchema {
        harn_vm::ProviderPayloadSchema::named("TriggerEvent")
    }

    fn client(&self) -> Arc<dyn harn_vm::ConnectorClient> {
        Arc::new(PlaceholderClient)
    }
}
