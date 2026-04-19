use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::sync::watch;

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
const MANIFEST_TOPIC: &str = "orchestrator.manifest";
const STATE_SNAPSHOT_FILE: &str = "orchestrator-state.json";
const PENDING_TOPIC: &str = "orchestrator.triggers.pending";
const CRON_TICK_TOPIC: &str = "connectors.cron.tick";

pub(crate) async fn run(args: OrchestratorServeArgs) -> Result<(), String> {
    let local = tokio::task::LocalSet::new();
    local.run_until(async move { run_local(args).await }).await
}

async fn run_local(args: OrchestratorServeArgs) -> Result<(), String> {
    harn_vm::reset_thread_local_state();
    let shutdown_timeout = Duration::from_secs(args.shutdown_timeout.max(1));

    let tls = TlsFiles::from_args(args.cert.clone(), args.key.clone())?;
    let config_path = absolutize_from_cwd(&args.local.config)?;
    let (manifest, manifest_dir) = load_manifest(&config_path)?;
    let state_dir = absolutize_from_cwd(&args.local.state_dir)?;
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

    let dispatcher = harn_vm::Dispatcher::with_event_log(vm, event_log.clone());
    let pending_pump = spawn_pending_pump(event_log.clone(), dispatcher.clone())?;
    let cron_pump = spawn_cron_pump(event_log.clone(), dispatcher.clone())?;
    let inbox_pump = spawn_inbox_pump(event_log.clone(), dispatcher.clone())?;

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
    let mut live_manifest = manifest;
    let mut live_triggers = collected_triggers;
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
            triggers: trigger_state_snapshots(&live_triggers, &listener_metrics),
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
            "trigger_count": live_triggers.len(),
            "connector_count": connector_runtime.providers.len(),
            "tls_enabled": listener.scheme() == "https",
            "shutdown_timeout_secs": shutdown_timeout.as_secs(),
        }),
    )
    .await?;

    wait_for_runtime_signal_loop(RuntimeSignalCtx {
        role: args.role,
        config_path: &config_path,
        state_dir: &state_dir,
        bind: local_bind,
        startup_started_at: &startup_started_at,
        event_log: &event_log,
        event_log_description: &event_log_description,
        secret_chain_display: &secret_chain_display,
        listener: &listener,
        connectors: &connector_runtime,
        live_manifest: &mut live_manifest,
        live_triggers: &mut live_triggers,
    })
    .await?;

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
            triggers: &live_triggers,
            connectors: &connector_runtime,
            shutdown_timeout,
        },
        listener,
        dispatcher,
        pending_pump,
        cron_pump,
        inbox_pump,
    )
    .await
}

struct ConnectorRuntime {
    _registry: harn_vm::ConnectorRegistry,
    handles: Vec<harn_vm::connectors::ConnectorHandle>,
    providers: Vec<String>,
    activations: Vec<harn_vm::ActivationHandle>,
}

struct PreparedManifestReload {
    manifest: Manifest,
    collected_triggers: Vec<CollectedManifestTrigger>,
    summary: ManifestReloadSummary,
}

#[derive(Debug, Default, Serialize)]
struct ManifestReloadSummary {
    added: Vec<String>,
    modified: Vec<String>,
    removed: Vec<String>,
    unchanged: Vec<String>,
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
        let binding = trigger_binding_for(&trigger.config)?;
        grouped_kinds
            .entry(binding.provider.clone())
            .or_default()
            .insert(binding.kind.as_str().to_string());
        trigger_registry.register(binding);
    }

    let metrics = Arc::new(harn_vm::MetricsRegistry::default());
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

    // `ConnectorRegistry::default()` pre-populates connectors for every
    // provider in the catalog (cron -> CronConnector, webhook-based ->
    // GenericWebhookConnector, github -> GitHubConnector, etc.). We only
    // need to register a PlaceholderConnector for providers that are
    // referenced by a trigger but *not* already in the catalog
    // (skip-if-already-registered).
    let mut providers = Vec::new();
    let mut handles = Vec::new();
    for (provider, kinds) in grouped_kinds {
        let provider_name = provider.as_str().to_string();
        if registry.get(&provider).is_none() {
            let connector = connector_for(&provider, kinds);
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

    let activations = registry
        .activate_all(&trigger_registry)
        .await
        .map_err(|error| error.to_string())?;

    Ok(ConnectorRuntime {
        _registry: registry,
        handles,
        providers,
        activations,
    })
}

fn trigger_binding_for(config: &ResolvedTriggerConfig) -> Result<harn_vm::TriggerBinding, String> {
    Ok(harn_vm::TriggerBinding {
        provider: config.provider.clone(),
        kind: harn_vm::TriggerKind::from(trigger_kind_name(config.kind)),
        binding_id: config.id.clone(),
        dedupe_key: config.dedupe_key.clone(),
        config: connector_binding_config(config)?,
    })
}

fn connector_binding_config(config: &ResolvedTriggerConfig) -> Result<JsonValue, String> {
    match config.kind {
        crate::package::TriggerKind::Cron => {
            serde_json::to_value(&config.kind_specific).map_err(|error| {
                format!(
                    "failed to encode cron trigger config '{}': {error}",
                    config.id
                )
            })
        }
        crate::package::TriggerKind::Webhook => Ok(serde_json::json!({
            "match": config.match_,
            "secrets": config.secrets,
            "webhook": config.kind_specific,
        })),
        _ => Ok(JsonValue::Null),
    }
    // Note: #220's earlier approach inserted `retention_days` into the JSON
    // binding config. Post-#221, dedupe retention is carried on the
    // TriggerBinding struct field `dedupe_retention_days` (see
    // triggers/registry.rs) rather than in the connector-specific JSON,
    // so no insertion is needed here.
}

fn connector_for(
    provider: &harn_vm::ProviderId,
    kinds: BTreeSet<String>,
) -> Box<dyn harn_vm::Connector> {
    match provider.as_str() {
        "cron" => Box::new(harn_vm::CronConnector::new()),
        _ => Box::new(PlaceholderConnector::new(provider.clone(), kinds)),
    }
}

fn build_route_configs(
    triggers: &[CollectedManifestTrigger],
    binding_versions: &BTreeMap<String, u32>,
) -> Result<Vec<RouteConfig>, String> {
    let mut seen_paths = BTreeSet::new();
    let mut routes = Vec::new();
    for trigger in triggers {
        let Some(binding_version) = binding_versions.get(&trigger.config.id).copied() else {
            return Err(format!(
                "trigger registry is missing active manifest binding '{}'",
                trigger.config.id
            ));
        };
        if let Some(route) = RouteConfig::from_trigger(trigger, binding_version)? {
            if !seen_paths.insert(route.path.clone()) {
                return Err(format!(
                    "trigger route '{}' is configured more than once",
                    route.path
                ));
            }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PumpMode {
    Running,
    Draining { up_to: u64 },
}

#[derive(Clone, Copy, Debug, Default)]
struct PumpStats {
    last_seen: u64,
    processed: u64,
}

struct PumpHandle {
    mode_tx: watch::Sender<PumpMode>,
    join: tokio::task::JoinHandle<Result<PumpStats, String>>,
}

impl PumpHandle {
    async fn drain(self, up_to: u64) -> Result<PumpStats, String> {
        let _ = self.mode_tx.send(PumpMode::Draining { up_to });
        match self.join.await {
            Ok(result) => result,
            Err(error) => Err(format!("pump task join failed: {error}")),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PendingTriggerRecord {
    trigger_id: String,
    binding_version: u32,
    event: harn_vm::TriggerEvent,
}

fn spawn_pending_pump(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    dispatcher: harn_vm::Dispatcher,
) -> Result<PumpHandle, String> {
    let topic = harn_vm::event_log::Topic::new(PENDING_TOPIC).map_err(|error| error.to_string())?;
    spawn_topic_pump(event_log, topic, move |logged| {
        let dispatcher = dispatcher.clone();
        async move {
            if logged.kind != "trigger_event" {
                return Ok(false);
            }
            let record: PendingTriggerRecord = serde_json::from_value(logged.payload)
                .map_err(|error| format!("failed to decode pending trigger event: {error}"))?;
            dispatcher
                .enqueue_targeted(
                    Some(record.trigger_id),
                    Some(record.binding_version),
                    record.event,
                )
                .await
                .map_err(|error| format!("failed to enqueue pending trigger event: {error}"))?;
            Ok(true)
        }
    })
}

fn spawn_cron_pump(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    dispatcher: harn_vm::Dispatcher,
) -> Result<PumpHandle, String> {
    let topic =
        harn_vm::event_log::Topic::new(CRON_TICK_TOPIC).map_err(|error| error.to_string())?;
    spawn_topic_pump(event_log, topic, move |logged| {
        let dispatcher = dispatcher.clone();
        async move {
            if logged.kind != "trigger_event" {
                return Ok(false);
            }
            let event: harn_vm::TriggerEvent = serde_json::from_value(logged.payload)
                .map_err(|error| format!("failed to decode cron trigger event: {error}"))?;
            let trigger_id = match &event.provider_payload {
                harn_vm::ProviderPayload::Known(
                    harn_vm::triggers::event::KnownProviderPayload::Cron(payload),
                ) => payload.cron_id.clone(),
                _ => None,
            };
            dispatcher
                .enqueue_targeted(trigger_id, None, event)
                .await
                .map_err(|error| format!("failed to enqueue cron trigger event: {error}"))?;
            Ok(true)
        }
    })
}

fn spawn_inbox_pump(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    dispatcher: harn_vm::Dispatcher,
) -> Result<PumpHandle, String> {
    let topic = harn_vm::event_log::Topic::new(harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC)
        .map_err(|error| error.to_string())?;
    spawn_topic_pump(event_log, topic, move |logged| {
        let dispatcher = dispatcher.clone();
        async move {
            if logged.kind != "event_ingested" {
                return Ok(false);
            }
            let envelope: harn_vm::triggers::dispatcher::InboxEnvelope =
                serde_json::from_value(logged.payload)
                    .map_err(|error| format!("failed to decode dispatcher inbox event: {error}"))?;
            tokio::task::spawn_local(async move {
                let _ = dispatcher.dispatch_inbox_envelope(envelope).await;
            });
            Ok(true)
        }
    })
}

fn spawn_topic_pump<F, Fut>(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    topic: harn_vm::event_log::Topic,
    process: F,
) -> Result<PumpHandle, String>
where
    F: Fn(harn_vm::event_log::LogEvent) -> Fut + 'static,
    Fut: std::future::Future<Output = Result<bool, String>> + 'static,
{
    let (mode_tx, mut mode_rx) = watch::channel(PumpMode::Running);
    let join = tokio::task::spawn_local(async move {
        let start_from = event_log
            .latest(&topic)
            .await
            .map_err(|error| format!("failed to read topic head {topic}: {error}"))?;
        let mut stream = event_log
            .clone()
            .subscribe(&topic, start_from)
            .await
            .map_err(|error| format!("failed to subscribe topic {topic}: {error}"))?;
        let mut stats = PumpStats {
            last_seen: start_from.unwrap_or(0),
            processed: 0,
        };
        loop {
            if matches!(*mode_rx.borrow(), PumpMode::Draining { up_to } if stats.last_seen >= up_to)
            {
                break;
            }
            tokio::select! {
                changed = mode_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                }
                received = stream.next() => {
                    let Some(received) = received else {
                        break;
                    };
                    let (event_id, logged) = received
                        .map_err(|error| format!("topic pump read failed for {topic}: {error}"))?;
                    let handled = process(logged).await?;
                    stats.last_seen = event_id;
                    if handled {
                        stats.processed += 1;
                    }
                }
            }
        }
        Ok(stats)
    });
    Ok(PumpHandle { mode_tx, join })
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
    dispatcher: harn_vm::Dispatcher,
    pending_pump: PumpHandle,
    cron_pump: PumpHandle,
    inbox_pump: PumpHandle,
) -> Result<(), String> {
    eprintln!("[harn] signal received, starting graceful shutdown...");
    let listener_in_flight = listener
        .trigger_metrics()
        .into_values()
        .map(|metrics| metrics.in_flight)
        .sum::<u64>();
    let dispatcher_before = dispatcher.snapshot();
    append_lifecycle_event(
        ctx.event_log,
        "draining",
        json!({
            "bind": ctx.bind.to_string(),
            "role": ctx.role.as_str(),
            "status": "draining",
            "http_in_flight": listener_in_flight,
            "dispatcher_in_flight": dispatcher_before.in_flight,
            "dispatcher_retry_queue_depth": dispatcher_before.retry_queue_depth,
            "dispatcher_dlq_depth": dispatcher_before.dlq_depth,
            "shutdown_timeout_secs": ctx.shutdown_timeout.as_secs(),
        }),
    )
    .await?;

    let deadline = tokio::time::Instant::now() + ctx.shutdown_timeout;
    let listener_metrics = listener.shutdown(remaining_budget(deadline)).await?;
    for handle in &ctx.connectors.handles {
        let connector = handle.lock().await;
        if let Err(error) = connector.shutdown(remaining_budget(deadline)).await {
            eprintln!(
                "[harn] connector {} shutdown warning: {error}",
                connector.provider_id().as_str()
            );
        }
    }

    let pending_stats = pending_pump
        .drain(topic_latest_id(ctx.event_log, PENDING_TOPIC).await?)
        .await?;
    let cron_stats = cron_pump
        .drain(topic_latest_id(ctx.event_log, CRON_TICK_TOPIC).await?)
        .await?;
    let inbox_stats = inbox_pump
        .drain(topic_latest_id(ctx.event_log, harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC).await?)
        .await?;
    let drain_report = dispatcher
        .drain(remaining_budget(deadline))
        .await
        .map_err(|error| format!("failed to drain dispatcher: {error}"))?;

    let stopped_at = now_rfc3339()?;
    let timed_out = !drain_report.drained;
    append_lifecycle_event(
        ctx.event_log,
        "stopped",
        json!({
            "bind": ctx.bind.to_string(),
            "role": ctx.role.as_str(),
            "status": "stopped",
            "http_in_flight": listener_in_flight,
            "dispatcher_in_flight": drain_report.in_flight,
            "dispatcher_retry_queue_depth": drain_report.retry_queue_depth,
            "dispatcher_dlq_depth": drain_report.dlq_depth,
            "pending_events_drained": pending_stats.processed,
            "cron_events_drained": cron_stats.processed,
            "inbox_events_drained": inbox_stats.processed,
            "timed_out": timed_out,
        }),
    )
    .await?;
    ctx.event_log
        .flush()
        .await
        .map_err(|error| format!("failed to flush event log: {error}"))?;

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

    if timed_out {
        eprintln!(
            "[harn] graceful shutdown timed out with {} dispatches and {} retry waits remaining",
            drain_report.in_flight, drain_report.retry_queue_depth
        );
    }
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

async fn append_manifest_event(
    log: &Arc<harn_vm::event_log::AnyEventLog>,
    kind: &str,
    payload: JsonValue,
) -> Result<(), String> {
    let topic =
        harn_vm::event_log::Topic::new(MANIFEST_TOPIC).map_err(|error| error.to_string())?;
    log.append(&topic, harn_vm::event_log::LogEvent::new(kind, payload))
        .await
        .map(|_| ())
        .map_err(|error| format!("failed to append orchestrator manifest event: {error}"))
}

async fn topic_latest_id(
    log: &Arc<harn_vm::event_log::AnyEventLog>,
    topic_name: &str,
) -> Result<u64, String> {
    let topic = harn_vm::event_log::Topic::new(topic_name).map_err(|error| error.to_string())?;
    log.latest(&topic)
        .await
        .map(|value| value.unwrap_or(0))
        .map_err(|error| format!("failed to read topic head for {topic_name}: {error}"))
}

fn remaining_budget(deadline: tokio::time::Instant) -> Duration {
    deadline.saturating_duration_since(tokio::time::Instant::now())
}

struct RuntimeSignalCtx<'a> {
    role: OrchestratorRole,
    config_path: &'a Path,
    state_dir: &'a Path,
    bind: SocketAddr,
    startup_started_at: &'a str,
    event_log: &'a Arc<harn_vm::event_log::AnyEventLog>,
    event_log_description: &'a harn_vm::event_log::EventLogDescription,
    secret_chain_display: &'a str,
    listener: &'a ListenerRuntime,
    connectors: &'a ConnectorRuntime,
    live_manifest: &'a mut Manifest,
    live_triggers: &'a mut Vec<CollectedManifestTrigger>,
}

async fn wait_for_runtime_signal_loop(ctx: RuntimeSignalCtx<'_>) -> Result<(), String> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm = signal(SignalKind::terminate())
            .map_err(|error| format!("failed to register SIGTERM handler: {error}"))?;
        let mut sigint = signal(SignalKind::interrupt())
            .map_err(|error| format!("failed to register SIGINT handler: {error}"))?;
        let mut sighup = signal(SignalKind::hangup())
            .map_err(|error| format!("failed to register SIGHUP handler: {error}"))?;
        loop {
            tokio::select! {
                _ = sigterm.recv() => return Ok(()),
                _ = sigint.recv() => return Ok(()),
                _ = sighup.recv() => {
                    match reload_manifest(&ctx).await {
                        Ok(reload) => {
                            *ctx.live_manifest = reload.manifest;
                            *ctx.live_triggers = reload.collected_triggers;
                            let listener_metrics = ctx.listener.trigger_metrics();
                            write_state_snapshot(
                                &ctx.state_dir.join(STATE_SNAPSHOT_FILE),
                                &ServeStateSnapshot {
                                    status: "running".to_string(),
                                    role: ctx.role.as_str().to_string(),
                                    bind: ctx.bind.to_string(),
                                    manifest_path: ctx.config_path.display().to_string(),
                                    state_dir: ctx.state_dir.display().to_string(),
                                    started_at: ctx.startup_started_at.to_string(),
                                    stopped_at: None,
                                    secret_provider_chain: ctx.secret_chain_display.to_string(),
                                    event_log_backend: ctx.event_log_description.backend.to_string(),
                                    event_log_location: ctx
                                        .event_log_description
                                        .location
                                        .as_ref()
                                        .map(|path| path.display().to_string()),
                                    triggers: trigger_state_snapshots(ctx.live_triggers, &listener_metrics),
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
                            append_manifest_event(
                                ctx.event_log,
                                "reload_succeeded",
                                serde_json::to_value(&reload.summary).unwrap_or_default(),
                            )
                            .await?;
                            eprintln!(
                                "[harn] manifest reload applied: +{} ~{} -{}",
                                reload.summary.added.len(),
                                reload.summary.modified.len(),
                                reload.summary.removed.len()
                            );
                        }
                        Err(error) => {
                            eprintln!("[harn] manifest reload failed: {error}");
                            append_manifest_event(
                                ctx.event_log,
                                "reload_failed",
                                json!({
                                    "error": error,
                                }),
                            )
                            .await?;
                        }
                    }
                }
            }
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .map_err(|error| format!("failed to wait for Ctrl-C: {error}"))
    }
}

async fn reload_manifest(ctx: &RuntimeSignalCtx<'_>) -> Result<PreparedManifestReload, String> {
    let (manifest, manifest_dir) = load_manifest(ctx.config_path)?;
    let mut vm = ctx
        .role
        .build_vm(&manifest_dir, &manifest_dir, ctx.state_dir)?;
    let extensions = package::load_runtime_extensions(ctx.config_path);
    let collected_triggers = package::collect_manifest_triggers(&mut vm, &extensions)
        .await
        .map_err(|error| format!("failed to collect manifest triggers: {error}"))?;
    ensure_reloadable_trigger_changes(ctx.live_triggers, &collected_triggers)?;
    let summary = summarize_manifest_reload(ctx.live_triggers, &collected_triggers);
    package::install_collected_manifest_triggers(&collected_triggers).await?;
    let binding_versions = live_manifest_binding_versions();
    let route_configs = build_route_configs(&collected_triggers, &binding_versions)?;
    ctx.listener.reload_routes(route_configs)?;
    Ok(PreparedManifestReload {
        manifest,
        collected_triggers,
        summary,
    })
}

fn ensure_reloadable_trigger_changes(
    current: &[CollectedManifestTrigger],
    next: &[CollectedManifestTrigger],
) -> Result<(), String> {
    let current_non_http = trigger_fingerprint_map(current, false);
    let next_non_http = trigger_fingerprint_map(next, false);
    if current_non_http != next_non_http {
        return Err(
            "SIGHUP reload currently supports manifest-backed HTTP triggers only; connector-managed trigger changes still require restart"
                .to_string(),
        );
    }
    Ok(())
}

fn summarize_manifest_reload(
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

fn trigger_fingerprint_map(
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

fn is_http_managed_trigger(trigger: &CollectedManifestTrigger) -> bool {
    matches!(
        trigger.config.kind,
        crate::package::TriggerKind::Webhook | crate::package::TriggerKind::A2aPush
    )
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
    shutdown_timeout: Duration,
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
