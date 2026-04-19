use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value as JsonValue};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use harn_vm::event_log::EventLog;

use super::listener::{ListenerConfig, ListenerRuntime, RouteConfig, TriggerMetricSnapshot};
use super::origin_guard::OriginAllowList;
use super::role::OrchestratorRole;
use super::tls::TlsFiles;
use crate::cli::OrchestratorServeArgs;
use crate::package::{
    self, CollectedManifestTrigger, CollectedTriggerHandler, Manifest, ResolvedTriggerConfig,
};

const LIFECYCLE_TOPIC: &str = "orchestrator.lifecycle";
const STATE_SNAPSHOT_FILE: &str = "orchestrator-state.json";

pub(crate) async fn run(args: OrchestratorServeArgs) -> Result<(), String> {
    harn_vm::reset_thread_local_state();

    let tls = TlsFiles::from_args(args.cert.clone(), args.key.clone())?;
    let config_path = absolutize_from_cwd(&args.config)?;
    let (manifest, manifest_dir) = load_manifest(&config_path)?;
    let state_dir = absolutize_from_cwd(&args.state_dir)?;
    std::fs::create_dir_all(&state_dir).map_err(|error| {
        format!(
            "failed to create state dir {}: {error}",
            state_dir.display()
        )
    })?;

    let workspace_root = manifest_dir.clone();
    let startup_started_at = now_rfc3339()?;

    eprintln!("[harn] orchestrator manifest: {}", config_path.display());
    if let Some(name) = manifest
        .package
        .as_ref()
        .and_then(|package| package.name.as_deref())
    {
        eprintln!("[harn] orchestrator package: {name}");
    }
    eprintln!(
        "[harn] orchestrator role: {} ({})",
        args.role.as_str(),
        args.role.registry_mode()
    );
    eprintln!("[harn] orchestrator state dir: {}", state_dir.display());

    let mut vm = args
        .role
        .build_vm(&workspace_root, &manifest_dir, &state_dir)?;

    let event_log = harn_vm::event_log::active_event_log()
        .ok_or_else(|| "event log was not installed during VM initialization".to_string())?;
    let event_log_description = event_log.describe();
    eprintln!(
        "[harn] event log: {} {}",
        event_log_description.backend,
        event_log_description
            .location
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<memory>".to_string())
    );

    let secret_namespace = secret_namespace_for(&manifest_dir);
    let secret_chain_display = configured_secret_chain_display();
    let secret_chain = harn_vm::secrets::configured_default_chain(secret_namespace.clone())
        .map_err(|error| format!("failed to configure secret providers: {error}"))?;
    if secret_chain.providers().is_empty() {
        return Err("secret provider chain resolved to zero providers".to_string());
    }
    eprintln!(
        "[harn] secret providers: {} (namespace {})",
        secret_chain_display, secret_namespace
    );
    let secret_provider: Arc<dyn harn_vm::secrets::SecretProvider> = Arc::new(secret_chain);

    let extensions = package::load_runtime_extensions(&config_path);
    let collected_triggers = package::collect_manifest_triggers(&mut vm, &extensions)
        .await
        .map_err(|error| format!("failed to collect manifest triggers: {error}"))?;
    package::install_collected_manifest_triggers(&collected_triggers).await?;
    eprintln!(
        "[harn] registered triggers ({}): {}",
        collected_triggers.len(),
        format_trigger_summary(&collected_triggers)
    );

    let binding_versions = live_manifest_binding_versions();
    let route_configs = build_route_configs(&collected_triggers, &binding_versions)?;
    let connector_runtime = initialize_connectors(
        &collected_triggers,
        event_log.clone(),
        secret_provider.clone(),
    )
    .await?;
    eprintln!(
        "[harn] registered connectors ({}): {}",
        connector_runtime.providers.len(),
        connector_runtime.providers.join(", ")
    );
    eprintln!(
        "[harn] activated connectors: {}",
        format_activation_summary(&connector_runtime.activations)
    );

    let listener = ListenerRuntime::start(ListenerConfig {
        bind: args.bind,
        tls,
        event_log: event_log.clone(),
        secrets: secret_provider.clone(),
        allowed_origins: OriginAllowList::from_manifest(&manifest.orchestrator.allowed_origins),
        max_body_bytes: ListenerConfig::max_body_bytes_or_default(
            manifest.orchestrator.max_body_bytes,
        ),
        routes: route_configs,
    })
    .await?;
    let local_bind = listener.local_addr();
    let listener_metrics = listener.trigger_metrics();
    eprintln!("[harn] HTTP listener ready on {}", listener.url());

    write_state_snapshot(
        &state_dir.join(STATE_SNAPSHOT_FILE),
        &ServeStateSnapshot {
            status: "running".to_string(),
            role: args.role.as_str().to_string(),
            bind: local_bind.to_string(),
            manifest_path: config_path.display().to_string(),
            state_dir: state_dir.display().to_string(),
            started_at: startup_started_at.clone(),
            stopped_at: None,
            secret_provider_chain: secret_chain_display.clone(),
            event_log_backend: event_log_description.backend.to_string(),
            event_log_location: event_log_description
                .location
                .as_ref()
                .map(|path| path.display().to_string()),
            triggers: trigger_state_snapshots(&collected_triggers, &listener_metrics),
            connectors: connector_runtime.providers.clone(),
            activations: connector_runtime
                .activations
                .iter()
                .map(|activation| ConnectorActivationSnapshot {
                    provider: activation.provider.as_str().to_string(),
                    binding_count: activation.binding_count,
                })
                .collect(),
        },
    )?;

    append_lifecycle_event(
        &event_log,
        "startup",
        json!({
            "bind": local_bind.to_string(),
            "manifest": config_path.display().to_string(),
            "role": args.role.as_str(),
            "state_dir": state_dir.display().to_string(),
            "trigger_count": collected_triggers.len(),
            "connector_count": connector_runtime.providers.len(),
            "tls_enabled": listener.scheme() == "https",
        }),
    )
    .await?;

    wait_for_termination_signal().await?;

    graceful_shutdown(
        GracefulShutdownCtx {
            role: args.role,
            bind: local_bind,
            config_path: &config_path,
            state_dir: &state_dir,
            startup_started_at: &startup_started_at,
            event_log: &event_log,
            event_log_description: &event_log_description,
            secret_chain_display: &secret_chain_display,
            triggers: &collected_triggers,
            connectors: &connector_runtime,
        },
        listener,
    )
    .await
}

struct ConnectorRuntime {
    _registry: harn_vm::ConnectorRegistry,
    providers: Vec<String>,
    activations: Vec<harn_vm::ActivationHandle>,
}

async fn initialize_connectors(
    triggers: &[CollectedManifestTrigger],
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    secrets: Arc<dyn harn_vm::secrets::SecretProvider>,
) -> Result<ConnectorRuntime, String> {
    let mut registry = harn_vm::ConnectorRegistry::default();
    let mut trigger_registry = harn_vm::TriggerRegistry::default();
    let mut grouped_kinds: BTreeMap<harn_vm::ProviderId, BTreeSet<String>> = BTreeMap::new();

    for trigger in triggers {
        let binding = trigger_binding_for(&trigger.config);
        grouped_kinds
            .entry(binding.provider.clone())
            .or_default()
            .insert(binding.kind.as_str().to_string());
        trigger_registry.register(binding);
    }

    let ctx = harn_vm::ConnectorCtx {
        event_log,
        secrets,
        inbox: Arc::new(harn_vm::InboxIndex::default()),
        metrics: Arc::new(harn_vm::MetricsRegistry),
        rate_limiter: Arc::new(harn_vm::RateLimiterFactory::default()),
    };

    let mut providers = Vec::new();
    for (provider, kinds) in grouped_kinds {
        let provider_name = provider.as_str().to_string();
        let connector: Box<dyn harn_vm::Connector> = if provider.as_str() == "cron" {
            Box::new(harn_vm::CronConnector::new())
        } else {
            Box::new(PlaceholderConnector::new(provider.clone(), kinds))
        };
        registry
            .register(connector)
            .map_err(|error| error.to_string())?;
        let handle = registry
            .get(&provider)
            .ok_or_else(|| format!("connector registry lost provider '{}'", provider.as_str()))?;
        handle
            .lock()
            .await
            .init(ctx.clone())
            .await
            .map_err(|error| error.to_string())?;
        providers.push(provider_name);
    }

    let activations = registry
        .activate_all(&trigger_registry)
        .await
        .map_err(|error| error.to_string())?;

    Ok(ConnectorRuntime {
        _registry: registry,
        providers,
        activations,
    })
}

fn trigger_binding_for(config: &ResolvedTriggerConfig) -> harn_vm::TriggerBinding {
    let mut binding = harn_vm::TriggerBinding::new(
        config.provider.clone(),
        trigger_kind_name(config.kind),
        config.id.clone(),
    );
    let mut binding_config = serde_json::Map::new();
    binding_config.insert(
        "match".to_string(),
        serde_json::to_value(&config.match_).unwrap_or(JsonValue::Null),
    );
    binding_config.insert(
        "secrets".to_string(),
        serde_json::to_value(&config.secrets).unwrap_or(JsonValue::Null),
    );
    for (key, value) in &config.kind_specific {
        binding_config.insert(
            key.clone(),
            serde_json::to_value(value).unwrap_or(JsonValue::Null),
        );
    }
    binding.config = JsonValue::Object(binding_config);
    binding
}

fn build_route_configs(
    triggers: &[CollectedManifestTrigger],
    binding_versions: &BTreeMap<String, u32>,
) -> Result<Vec<RouteConfig>, String> {
    let mut routes = Vec::new();
    for trigger in triggers {
        let Some(binding_version) = binding_versions.get(&trigger.config.id).copied() else {
            return Err(format!(
                "trigger registry is missing active manifest binding '{}'",
                trigger.config.id
            ));
        };
        if let Some(route) = RouteConfig::from_trigger(trigger, binding_version)? {
            routes.push(route);
        }
    }
    Ok(routes)
}

fn live_manifest_binding_versions() -> BTreeMap<String, u32> {
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

fn trigger_state_snapshots(
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

fn format_trigger_summary(triggers: &[CollectedManifestTrigger]) -> String {
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

fn format_activation_summary(activations: &[harn_vm::ActivationHandle]) -> String {
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

fn handler_kind(handler: &CollectedTriggerHandler) -> &'static str {
    match handler {
        CollectedTriggerHandler::Local { .. } => "local",
        CollectedTriggerHandler::A2a { .. } => "a2a",
        CollectedTriggerHandler::Worker { .. } => "worker",
    }
}

fn trigger_kind_name(kind: crate::package::TriggerKind) -> &'static str {
    match kind {
        crate::package::TriggerKind::Webhook => "webhook",
        crate::package::TriggerKind::Cron => "cron",
        crate::package::TriggerKind::Poll => "poll",
        crate::package::TriggerKind::Stream => "stream",
        crate::package::TriggerKind::Predicate => "predicate",
        crate::package::TriggerKind::A2aPush => "a2a-push",
    }
}

async fn graceful_shutdown(
    ctx: GracefulShutdownCtx<'_>,
    listener: ListenerRuntime,
) -> Result<(), String> {
    eprintln!("[harn] signal received, starting graceful shutdown...");
    // TODO(O-06): replace this listener-local scan with the shared
    // orchestrator drain coordinator when that lands on the main branch.
    let drained_events = listener
        .trigger_metrics()
        .into_values()
        .map(|metrics| metrics.in_flight)
        .sum::<u64>();
    let listener_metrics = listener.shutdown().await?;

    let stopped_at = now_rfc3339()?;
    append_lifecycle_event(
        ctx.event_log,
        "shutdown",
        json!({
            "bind": ctx.bind.to_string(),
            "drained_events": drained_events,
            "role": ctx.role.as_str(),
            "status": "stopped",
        }),
    )
    .await?;

    write_state_snapshot(
        &ctx.state_dir.join(STATE_SNAPSHOT_FILE),
        &ServeStateSnapshot {
            status: "stopped".to_string(),
            role: ctx.role.as_str().to_string(),
            bind: ctx.bind.to_string(),
            manifest_path: ctx.config_path.display().to_string(),
            state_dir: ctx.state_dir.display().to_string(),
            started_at: ctx.startup_started_at.to_string(),
            stopped_at: Some(stopped_at),
            secret_provider_chain: ctx.secret_chain_display.to_string(),
            event_log_backend: ctx.event_log_description.backend.to_string(),
            event_log_location: ctx
                .event_log_description
                .location
                .as_ref()
                .map(|path| path.display().to_string()),
            triggers: trigger_state_snapshots(ctx.triggers, &listener_metrics),
            connectors: ctx.connectors.providers.clone(),
            activations: ctx
                .connectors
                .activations
                .iter()
                .map(|activation| ConnectorActivationSnapshot {
                    provider: activation.provider.as_str().to_string(),
                    binding_count: activation.binding_count,
                })
                .collect(),
        },
    )?;

    eprintln!("[harn] graceful shutdown complete");
    Ok(())
}

async fn append_lifecycle_event(
    log: &Arc<harn_vm::event_log::AnyEventLog>,
    kind: &str,
    payload: JsonValue,
) -> Result<(), String> {
    let topic =
        harn_vm::event_log::Topic::new(LIFECYCLE_TOPIC).map_err(|error| error.to_string())?;
    log.append(&topic, harn_vm::event_log::LogEvent::new(kind, payload))
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to append orchestrator lifecycle event: {error}"))
}

async fn wait_for_termination_signal() -> Result<(), String> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm = signal(SignalKind::terminate())
            .map_err(|error| format!("failed to register SIGTERM handler: {error}"))?;
        let mut sigint = signal(SignalKind::interrupt())
            .map_err(|error| format!("failed to register SIGINT handler: {error}"))?;
        tokio::select! {
            _ = sigterm.recv() => Ok(()),
            _ = sigint.recv() => Ok(()),
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .map_err(|error| format!("failed to wait for Ctrl-C: {error}"))
    }
}

fn load_manifest(config_path: &Path) -> Result<(Manifest, PathBuf), String> {
    if !config_path.is_file() {
        return Err(format!("manifest not found: {}", config_path.display()));
    }
    let content = std::fs::read_to_string(config_path)
        .map_err(|error| format!("failed to read {}: {error}", config_path.display()))?;
    let manifest = toml::from_str::<Manifest>(&content)
        .map_err(|error| format!("failed to parse {}: {error}", config_path.display()))?;
    let manifest_dir = config_path.parent().map(Path::to_path_buf).ok_or_else(|| {
        format!(
            "manifest has no parent directory: {}",
            config_path.display()
        )
    })?;
    Ok((manifest, manifest_dir))
}

fn absolutize_from_cwd(path: &Path) -> Result<PathBuf, String> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("failed to read current directory: {error}"))?
            .join(path)
    };
    Ok(candidate)
}

fn configured_secret_chain_display() -> String {
    std::env::var(harn_vm::secrets::SECRET_PROVIDER_CHAIN_ENV)
        .unwrap_or_else(|_| harn_vm::secrets::DEFAULT_SECRET_PROVIDER_CHAIN.to_string())
        .split(',')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" -> ")
}

fn secret_namespace_for(manifest_dir: &Path) -> String {
    match std::env::var("HARN_SECRET_NAMESPACE") {
        Ok(namespace) if !namespace.trim().is_empty() => namespace,
        _ => {
            let leaf = manifest_dir
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or("workspace");
            format!("harn/{leaf}")
        }
    }
}

fn now_rfc3339() -> Result<String, String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| format!("failed to format timestamp: {error}"))
}

fn write_state_snapshot(path: &Path, snapshot: &ServeStateSnapshot) -> Result<(), String> {
    let encoded = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| format!("failed to encode orchestrator state snapshot: {error}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    std::fs::write(path, encoded)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))
}

struct GracefulShutdownCtx<'a> {
    role: OrchestratorRole,
    bind: SocketAddr,
    config_path: &'a Path,
    state_dir: &'a Path,
    startup_started_at: &'a str,
    event_log: &'a Arc<harn_vm::event_log::AnyEventLog>,
    event_log_description: &'a harn_vm::event_log::EventLogDescription,
    secret_chain_display: &'a str,
    triggers: &'a [CollectedManifestTrigger],
    connectors: &'a ConnectorRuntime,
}

#[derive(Debug, Serialize)]
struct ServeStateSnapshot {
    status: String,
    role: String,
    bind: String,
    manifest_path: String,
    state_dir: String,
    started_at: String,
    stopped_at: Option<String>,
    secret_provider_chain: String,
    event_log_backend: String,
    event_log_location: Option<String>,
    triggers: Vec<TriggerStateSnapshot>,
    connectors: Vec<String>,
    activations: Vec<ConnectorActivationSnapshot>,
}

#[derive(Debug, Serialize)]
struct TriggerStateSnapshot {
    id: String,
    provider: String,
    kind: String,
    handler: String,
    version: Option<u32>,
    state: Option<String>,
    received: u64,
    dispatched: u64,
    failed: u64,
    in_flight: u64,
}

#[derive(Debug, Serialize)]
struct ConnectorActivationSnapshot {
    provider: String,
    binding_count: usize,
}

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

    fn normalize_inbound(
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
