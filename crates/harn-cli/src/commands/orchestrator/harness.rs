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
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

use harn_vm::event_log::{AnyEventLog, ConsumerId, EventLog};

use super::common::stranded_envelopes;
use super::errors::OrchestratorError;
use super::listener::{
    AdminReloadHandle, AdminReloadRequest, ListenerConfig, ListenerRuntime, RouteConfig,
    TriggerMetricSnapshot,
};
use super::origin_guard::OriginAllowList;
use super::role::OrchestratorRole;
use super::supervisor_state::apply_supervisor_state;
use super::tls::TlsFiles;
use crate::package::{
    self, CollectedManifestTrigger, CollectedTriggerHandler, Manifest,
    ResolvedProviderConnectorConfig, ResolvedProviderConnectorKind, ResolvedTriggerConfig,
};

const LIFECYCLE_TOPIC: &str = "orchestrator.lifecycle";
#[cfg_attr(not(unix), allow(dead_code))]
const MANIFEST_TOPIC: &str = "orchestrator.manifest";
const STATE_SNAPSHOT_FILE: &str = "orchestrator-state.json";
const PENDING_TOPIC: &str = "orchestrator.triggers.pending";
const CRON_TICK_TOPIC: &str = "connectors.cron.tick";
const TEST_INBOX_TASK_RELEASE_FILE_ENV: &str = "HARN_TEST_ORCHESTRATOR_INBOX_TASK_RELEASE_FILE";
const TEST_FAIL_PENDING_PUMP_ENV: &str = "HARN_TEST_ORCHESTRATOR_FAIL_PENDING_PUMP";
const WAITPOINT_SERVICE_INTERVAL: Duration = Duration::from_millis(250);

// ── Public config ─────────────────────────────────────────────────────────────

/// Drain limits applied during graceful shutdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrainConfig {
    pub max_items: usize,
    pub deadline: Duration,
}

impl Default for DrainConfig {
    fn default() -> Self {
        Self {
            max_items: crate::package::default_orchestrator_drain_max_items(),
            deadline: Duration::from_secs(
                crate::package::default_orchestrator_drain_deadline_seconds(),
            ),
        }
    }
}

/// Topic-pump concurrency limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PumpConfig {
    pub max_outstanding: usize,
}

impl Default for PumpConfig {
    fn default() -> Self {
        Self {
            max_outstanding: crate::package::default_orchestrator_pump_max_outstanding(),
        }
    }
}

/// Configuration for `OrchestratorHarness::start`.
#[derive(Clone, Debug)]
pub struct OrchestratorConfig {
    pub manifest_path: PathBuf,
    pub state_dir: PathBuf,
    pub bind: SocketAddr,
    pub role: OrchestratorRole,
    pub watch_manifest: bool,
    pub mcp: bool,
    pub mcp_path: String,
    pub mcp_sse_path: String,
    pub mcp_messages_path: String,
    pub tls: Option<TlsFiles>,
    pub shutdown_timeout: Duration,
    pub drain: DrainConfig,
    pub pump: PumpConfig,
    /// When `Some`, installs an observability tracing subscriber.  Tests
    /// should leave this `None` to avoid conflicts with the test runtime.
    pub log_format: Option<harn_vm::observability::otel::LogFormat>,
}

impl OrchestratorConfig {
    /// Minimal defaults suitable for integration tests.
    pub fn for_test(manifest_path: PathBuf, state_dir: PathBuf) -> Self {
        Self {
            manifest_path,
            state_dir,
            bind: "127.0.0.1:0".parse().unwrap(),
            role: OrchestratorRole::SingleTenant,
            watch_manifest: false,
            mcp: false,
            mcp_path: "/mcp".to_string(),
            mcp_sse_path: "/sse".to_string(),
            mcp_messages_path: "/messages".to_string(),
            tls: None,
            shutdown_timeout: Duration::from_secs(5),
            drain: DrainConfig::default(),
            pump: PumpConfig::default(),
            log_format: None,
        }
    }
}

// ── Public harness ────────────────────────────────────────────────────────────

/// Summary returned by `OrchestratorHarness::shutdown`.
#[derive(Debug)]
pub struct ShutdownReport {
    #[allow(dead_code)]
    pub timed_out: bool,
}

/// Error type for harness operations.
#[derive(Debug)]
pub struct HarnessError(pub String);

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<OrchestratorError> for HarnessError {
    fn from(error: OrchestratorError) -> Self {
        HarnessError(error.to_string())
    }
}

impl From<String> for HarnessError {
    fn from(s: String) -> Self {
        HarnessError(s)
    }
}

/// In-process orchestrator runtime.
///
/// Starts all pumps, connectors, and the HTTP listener in a dedicated
/// background thread with its own single-threaded Tokio runtime + LocalSet.
/// Tests hold `Arc<AnyEventLog>` directly and use `EventLog::subscribe()` for
/// event-driven waits instead of polling.
#[allow(dead_code)]
pub struct OrchestratorHarness {
    event_log: Arc<AnyEventLog>,
    listener_url: String,
    local_addr: SocketAddr,
    state_dir: PathBuf,
    admin_reload: AdminReloadHandle,
    shutdown_tx: Arc<watch::Sender<bool>>,
    pump_drain_gate: PumpDrainGate,
    join: Option<std::thread::JoinHandle<()>>,
}

struct ReadyState {
    event_log: Arc<AnyEventLog>,
    listener_url: String,
    local_addr: SocketAddr,
    state_dir: PathBuf,
    admin_reload: AdminReloadHandle,
}

#[derive(Clone)]
struct PumpDrainGate {
    hold_tx: watch::Sender<bool>,
}

impl PumpDrainGate {
    fn new() -> Self {
        let (hold_tx, _) = watch::channel(false);
        Self { hold_tx }
    }

    fn pause(&self) {
        let _ = self.hold_tx.send(true);
    }

    fn release(&self) {
        let _ = self.hold_tx.send(false);
    }

    fn subscribe(&self) -> watch::Receiver<bool> {
        self.hold_tx.subscribe()
    }
}

#[allow(dead_code)]
impl OrchestratorHarness {
    /// Start the orchestrator in-process.  Resolves once the HTTP listener
    /// is ready and the startup lifecycle event has been appended.
    pub async fn start(config: OrchestratorConfig) -> Result<Self, HarnessError> {
        let (ready_tx, ready_rx) = oneshot::channel::<Result<ReadyState, OrchestratorError>>();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let shutdown_tx = Arc::new(shutdown_tx);
        let pump_drain_gate = PumpDrainGate::new();
        let task_pump_drain_gate = pump_drain_gate.clone();

        let join = std::thread::spawn(move || {
            // Use a multi-thread runtime so that blocking I/O (e.g. the OTEL
            // SimpleSpanProcessor calling futures::executor::block_on for each
            // span) can proceed on reactor threads while the LocalSet thread is
            // temporarily paused.  A current-thread runtime would deadlock:
            // block_on holds the only thread while the reactor needs that same
            // thread to drive the TCP export.
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("failed to build OrchestratorHarness tokio runtime");
            let local = tokio::task::LocalSet::new();
            rt.block_on(local.run_until(orchestrator_task(
                config,
                ready_tx,
                shutdown_rx,
                task_pump_drain_gate,
            )));
        });

        match ready_rx.await {
            Ok(Ok(ready)) => Ok(Self {
                event_log: ready.event_log,
                listener_url: ready.listener_url,
                local_addr: ready.local_addr,
                state_dir: ready.state_dir,
                admin_reload: ready.admin_reload,
                shutdown_tx,
                pump_drain_gate,
                join: Some(join),
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(HarnessError::from(error))
            }
            Err(_) => {
                let _ = join.join();
                Err(HarnessError(
                    "harness thread exited before signaling readiness".to_string(),
                ))
            }
        }
    }

    pub fn listener_url(&self) -> &str {
        &self.listener_url
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn event_log(&self) -> Arc<AnyEventLog> {
        self.event_log.clone()
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Returns a handle that can be used to trigger admin-reload from outside
    /// the harness (e.g. on SIGHUP in the CLI wrapper).
    pub fn admin_reload(&self) -> AdminReloadHandle {
        self.admin_reload.clone()
    }

    /// Returns a sender that, when `send(true)` is called, triggers graceful
    /// shutdown.  Use this to wire OS signal handlers in the CLI wrapper.
    pub fn shutdown_trigger(&self) -> Arc<watch::Sender<bool>> {
        self.shutdown_tx.clone()
    }

    /// Pause topic pumps at the next event admission. Tests can wait for
    /// `pump_drain_waiting` before triggering shutdown.
    pub fn pause_pump_drain(&self) {
        self.pump_drain_gate.pause();
    }

    /// Release topic pumps paused by `pause_pump_drain`.
    pub fn release_pump_drain(&self) {
        self.pump_drain_gate.release();
    }

    /// Cancel-safe, idempotent.  Performs the same drain logic as SIGTERM.
    pub async fn shutdown(mut self, _deadline: Duration) -> Result<ShutdownReport, HarnessError> {
        // Signal the background runtime to start graceful shutdown.
        let _ = self.shutdown_tx.send(true);
        // Wait for the background thread to finish (graceful shutdown runs there).
        let join = self.join.take().expect("join handle");
        tokio::task::spawn_blocking(move || join.join())
            .await
            .map_err(|_| HarnessError("spawn_blocking join failed".to_string()))?
            .map_err(|_| HarnessError("harness background thread panicked".to_string()))?;
        Ok(ShutdownReport { timed_out: false })
    }
}

impl Drop for OrchestratorHarness {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

// ── Internal lifecycle ────────────────────────────────────────────────────────

async fn orchestrator_task(
    config: OrchestratorConfig,
    ready_tx: oneshot::Sender<Result<ReadyState, OrchestratorError>>,
    shutdown_rx: watch::Receiver<bool>,
    pump_drain_gate: PumpDrainGate,
) {
    if let Err(error) = orchestrator_lifecycle(config, ready_tx, shutdown_rx, pump_drain_gate).await
    {
        eprintln!("[harn] orchestrator harness error: {error}");
    }
}

async fn orchestrator_lifecycle(
    config: OrchestratorConfig,
    ready_tx: oneshot::Sender<Result<ReadyState, OrchestratorError>>,
    mut shutdown_rx: watch::Receiver<bool>,
    pump_drain_gate: PumpDrainGate,
) -> Result<(), OrchestratorError> {
    harn_vm::reset_thread_local_state();

    let shutdown_timeout = config.shutdown_timeout;
    let drain_config = config.drain;
    let pump_config = PumpConfig {
        max_outstanding: config.pump.max_outstanding.max(1),
    };

    let state_dir = config.state_dir.clone();
    std::fs::create_dir_all(&state_dir).map_err(|error| {
        format!(
            "failed to create state dir {}: {error}",
            state_dir.display()
        )
    })?;

    let observability = if let Some(log_format) = config.log_format {
        Some(
            harn_vm::observability::otel::ObservabilityGuard::install_orchestrator_subscriber(
                harn_vm::observability::otel::OrchestratorObservabilityConfig {
                    log_format,
                    state_dir: Some(state_dir.clone()),
                },
            )?,
        )
    } else {
        None
    };

    let config_path = absolutize_from_cwd(&config.manifest_path)?;
    let (manifest, manifest_dir) = load_manifest(&config_path)?;
    let drain_config = DrainConfig {
        max_items: drain_config.max_items.max(1),
        deadline: drain_config.deadline,
    };
    let pump_config = PumpConfig {
        max_outstanding: pump_config.max_outstanding.max(1),
    };

    let startup_started_at = now_rfc3339()?;
    let (admin_reload, mut reload_rx) = AdminReloadHandle::channel();

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
        config.role.as_str(),
        config.role.registry_mode()
    );
    eprintln!("[harn] orchestrator state dir: {}", state_dir.display());
    tracing::info!(
        component = "orchestrator",
        trace_id = "",
        role = config.role.as_str(),
        state_dir = %state_dir.display(),
        manifest = %config_path.display(),
        "orchestrator starting"
    );

    let workspace_root = manifest_dir.clone();
    let mut vm = config
        .role
        .build_vm(&workspace_root, &manifest_dir, &state_dir)?;

    let event_log = harn_vm::event_log::active_event_log()
        .ok_or_else(|| "event log was not installed during VM initialization".to_string())?;
    let event_log_description = event_log.describe();
    let tenant_store = if config.role == OrchestratorRole::MultiTenant {
        let store = harn_vm::TenantStore::load(&state_dir)?;
        let active_tenants = store
            .list()
            .into_iter()
            .filter(|tenant| tenant.status == harn_vm::TenantStatus::Active)
            .collect::<Vec<_>>();
        eprintln!(
            "[harn] tenants loaded: {} active ({})",
            active_tenants.len(),
            active_tenants
                .iter()
                .map(|tenant| tenant.scope.id.0.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        Some(Arc::new(store))
    } else {
        None
    };
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
        return Err("secret provider chain resolved to zero providers"
            .to_string()
            .into());
    }
    eprintln!(
        "[harn] secret providers: {} (namespace {})",
        secret_chain_display, secret_namespace
    );
    let secret_provider: Arc<dyn harn_vm::secrets::SecretProvider> = Arc::new(secret_chain);

    let extensions = package::load_runtime_extensions(&config_path);
    let metrics_registry = Arc::new(harn_vm::MetricsRegistry::default());
    harn_vm::install_active_metrics_registry(metrics_registry.clone());
    let collected_triggers = package::collect_manifest_triggers(&mut vm, &extensions)
        .await
        .map_err(|error| format!("failed to collect manifest triggers: {error}"))?;
    package::install_collected_manifest_triggers(&collected_triggers).await?;
    apply_supervisor_state(&state_dir).await?;
    eprintln!(
        "[harn] registered triggers ({}): {}",
        collected_triggers.len(),
        format_trigger_summary(&collected_triggers)
    );

    let binding_versions = live_manifest_binding_versions();
    let route_configs = build_route_configs(&collected_triggers, &binding_versions)?;
    let mut connector_runtime = initialize_connectors(
        &collected_triggers,
        event_log.clone(),
        secret_provider.clone(),
        metrics_registry.clone(),
        &extensions.provider_connectors,
    )
    .await?;
    let route_configs = attach_route_connectors(
        route_configs,
        &connector_runtime.registry,
        &extensions.provider_connectors,
    )?;
    let connector_clients = connector_runtime.registry.client_map().await;
    harn_vm::install_active_connector_clients(connector_clients);
    eprintln!(
        "[harn] registered connectors ({}): {}",
        connector_runtime.providers.len(),
        connector_runtime.providers.join(", ")
    );
    eprintln!(
        "[harn] activated connectors: {}",
        format_activation_summary(&connector_runtime.activations)
    );
    let (mcp_router, mcp_service) = if config.mcp {
        validate_mcp_paths(
            &config.mcp_path,
            &config.mcp_sse_path,
            &config.mcp_messages_path,
        )?;
        if !has_orchestrator_api_keys_configured() && !has_mcp_oauth_configured() {
            return Err(OrchestratorError::Serve(
                "--mcp requires HARN_ORCHESTRATOR_API_KEYS or HARN_MCP_OAUTH_AUTHORIZATION_SERVERS so the embedded MCP management surface is authenticated"
                    .to_string(),
            ));
        }
        let service = Arc::new(
            crate::commands::mcp::serve::McpOrchestratorService::new_local(
                crate::cli::OrchestratorLocalArgs {
                    config: config_path.clone(),
                    state_dir: state_dir.clone(),
                },
            )?,
        );
        let router = crate::commands::mcp::serve::http_router_for_service(
            service.clone(),
            config.mcp_path.clone(),
            config.mcp_sse_path.clone(),
            config.mcp_messages_path.clone(),
        );
        eprintln!(
            "[harn] embedded MCP server mounted at {} (legacy SSE {}, messages {})",
            config.mcp_path, config.mcp_sse_path, config.mcp_messages_path
        );
        (Some(router), Some(service))
    } else {
        (None, None)
    };

    let dispatcher = harn_vm::Dispatcher::with_event_log_and_metrics(
        vm,
        event_log.clone(),
        Some(metrics_registry.clone()),
    );
    let mut pending_pumps = vec![(
        PENDING_TOPIC.to_string(),
        spawn_pending_pump(
            event_log.clone(),
            dispatcher.clone(),
            pump_config,
            metrics_registry.clone(),
            pump_drain_gate.clone(),
            PENDING_TOPIC,
        )?,
    )];
    let mut inbox_pumps = vec![(
        harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC.to_string(),
        spawn_inbox_pump(
            event_log.clone(),
            dispatcher.clone(),
            pump_config,
            metrics_registry.clone(),
            harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC,
        )?,
    )];
    if let Some(store) = tenant_store.as_ref() {
        for tenant in store
            .list()
            .into_iter()
            .filter(|tenant| tenant.status == harn_vm::TenantStatus::Active)
        {
            let pending_topic = harn_vm::tenant_topic(
                &tenant.scope.id,
                &harn_vm::event_log::Topic::new(PENDING_TOPIC)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            pending_pumps.push((
                pending_topic.as_str().to_string(),
                spawn_pending_pump(
                    event_log.clone(),
                    dispatcher.clone(),
                    pump_config,
                    metrics_registry.clone(),
                    pump_drain_gate.clone(),
                    pending_topic.as_str(),
                )?,
            ));
            let inbox_topic = harn_vm::tenant_topic(
                &tenant.scope.id,
                &harn_vm::event_log::Topic::new(harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC)
                    .map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            inbox_pumps.push((
                inbox_topic.as_str().to_string(),
                spawn_inbox_pump(
                    event_log.clone(),
                    dispatcher.clone(),
                    pump_config,
                    metrics_registry.clone(),
                    inbox_topic.as_str(),
                )?,
            ));
        }
    }
    let cron_pump = spawn_cron_pump(
        event_log.clone(),
        dispatcher.clone(),
        pump_config,
        metrics_registry.clone(),
        pump_drain_gate.clone(),
    )?;
    let waitpoint_pump = spawn_waitpoint_resume_pump(
        event_log.clone(),
        dispatcher.clone(),
        pump_config,
        metrics_registry.clone(),
        pump_drain_gate.clone(),
    )?;
    let waitpoint_cancel_pump = spawn_waitpoint_cancel_pump(
        event_log.clone(),
        dispatcher.clone(),
        pump_config,
        metrics_registry.clone(),
        pump_drain_gate.clone(),
    )?;
    let waitpoint_sweeper = spawn_waitpoint_sweeper(dispatcher.clone());

    let listener = ListenerRuntime::start(ListenerConfig {
        bind: config.bind,
        tls: config.tls.clone(),
        event_log: event_log.clone(),
        secrets: secret_provider.clone(),
        allowed_origins: OriginAllowList::from_manifest(&manifest.orchestrator.allowed_origins),
        max_body_bytes: ListenerConfig::max_body_bytes_or_default(
            manifest.orchestrator.max_body_bytes,
        ),
        metrics_registry: metrics_registry.clone(),
        admin_reload: Some(admin_reload.clone()),
        mcp_router,
        routes: route_configs,
        tenant_store: tenant_store.clone(),
        session_store: Some(Arc::new(harn_vm::SessionStore::new(event_log.clone()))),
    })
    .await?;
    let local_bind = listener.local_addr();
    let listener_metrics = listener.trigger_metrics();
    let mut live_manifest = manifest;
    let mut live_triggers = collected_triggers;
    let _manifest_watcher = if config.watch_manifest {
        Some(spawn_manifest_watcher(
            config_path.clone(),
            admin_reload.clone(),
        )?)
    } else {
        None
    };
    connector_runtime.activations = connector_runtime
        .registry
        .activate_all(&connector_runtime.trigger_registry)
        .await
        .map_err(|error| error.to_string())?;
    eprintln!(
        "[harn] activated connectors: {}",
        format_activation_summary(&connector_runtime.activations)
    );

    listener.mark_ready();
    eprintln!("[harn] HTTP listener ready on {}", listener.url());
    tracing::info!(
        component = "orchestrator",
        trace_id = "",
        listener_url = %listener.url(),
        "HTTP listener ready"
    );

    write_state_snapshot(
        &state_dir.join(STATE_SNAPSHOT_FILE),
        &ServeStateSnapshot {
            status: "running".to_string(),
            role: config.role.as_str().to_string(),
            bind: local_bind.to_string(),
            listener_url: listener.url(),
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
            "role": config.role.as_str(),
            "state_dir": state_dir.display().to_string(),
            "trigger_count": live_triggers.len(),
            "connector_count": connector_runtime.providers.len(),
            "tls_enabled": listener.scheme() == "https",
            "shutdown_timeout_secs": shutdown_timeout.as_secs(),
            "drain_max_items": drain_config.max_items,
            "drain_deadline_secs": drain_config.deadline.as_secs(),
            "pump_max_outstanding": pump_config.max_outstanding,
        }),
    )
    .await?;

    let stranded = stranded_envelopes(&event_log, Duration::ZERO).await?;
    if !stranded.is_empty() {
        eprintln!(
            "[harn] startup found {} stranded inbox envelope(s); inspect with `harn orchestrator queue` and recover explicitly with `harn orchestrator recover --dry-run --envelope-age ...`",
            stranded.len()
        );
    }
    append_lifecycle_event(
        &event_log,
        "startup_stranded_envelopes",
        json!({
            "count": stranded.len(),
        }),
    )
    .await?;

    // Signal that the harness is ready.
    let _ = ready_tx.send(Ok(ReadyState {
        event_log: event_log.clone(),
        listener_url: listener.url(),
        local_addr: local_bind,
        state_dir: state_dir.clone(),
        admin_reload: admin_reload.clone(),
    }));

    // Wait for shutdown trigger or admin reload requests.
    let mut ctx = RuntimeCtx {
        role: config.role,
        config_path: &config_path,
        state_dir: &state_dir,
        bind: local_bind,
        startup_started_at: &startup_started_at,
        event_log: &event_log,
        event_log_description: &event_log_description,
        secret_chain_display: &secret_chain_display,
        listener: &listener,
        connectors: &mut connector_runtime,
        live_manifest: &mut live_manifest,
        live_triggers: &mut live_triggers,
        secret_provider: &secret_provider,
        metrics_registry: &metrics_registry,
        mcp_service: mcp_service.as_ref(),
        reload_rx: &mut reload_rx,
    };

    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    break;
                }
            }
            Some(request) = ctx.reload_rx.recv() => {
                handle_reload_request(&mut ctx, request).await?;
            }
        }
    }

    listener.mark_not_ready();
    let shutdown = graceful_shutdown(
        GracefulShutdownCtx {
            role: config.role,
            bind: local_bind,
            listener_url: listener.url(),
            config_path: &config_path,
            state_dir: &state_dir,
            startup_started_at: &startup_started_at,
            event_log: &event_log,
            event_log_description: &event_log_description,
            secret_chain_display: &secret_chain_display,
            triggers: &live_triggers,
            connectors: &connector_runtime,
            shutdown_timeout,
            drain_config,
        },
        listener,
        dispatcher,
        pending_pumps,
        cron_pump,
        inbox_pumps,
        waitpoint_pump,
        waitpoint_cancel_pump,
        waitpoint_sweeper,
    )
    .await;

    if let Some(obs) = observability {
        if let Err(error) = obs.shutdown() {
            if shutdown.is_ok() {
                return Err(OrchestratorError::Serve(error));
            }
            eprintln!("[harn] observability shutdown warning: {error}");
        }
    }
    harn_vm::clear_active_metrics_registry();
    shutdown
}

// ── Internal types ────────────────────────────────────────────────────────────

struct ConnectorRuntime {
    registry: harn_vm::ConnectorRegistry,
    trigger_registry: harn_vm::TriggerRegistry,
    handles: Vec<harn_vm::connectors::ConnectorHandle>,
    providers: Vec<String>,
    activations: Vec<harn_vm::ActivationHandle>,
    #[cfg_attr(not(unix), allow(dead_code))]
    provider_overrides: Vec<ResolvedProviderConnectorConfig>,
}

#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Clone, Debug, Default, Serialize)]
struct ManifestReloadSummary {
    added: Vec<String>,
    modified: Vec<String>,
    removed: Vec<String>,
    unchanged: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PumpMode {
    Running,
    Draining(PumpDrainRequest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PumpDrainRequest {
    up_to: u64,
    config: DrainConfig,
    deadline: tokio::time::Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PumpDrainStopReason {
    Drained,
    MaxItems,
    Deadline,
    Error,
}

impl PumpDrainStopReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Drained => "drained",
            Self::MaxItems => "max_items",
            Self::Deadline => "deadline",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct PumpStats {
    last_seen: u64,
    processed: u64,
}

#[derive(Clone, Copy, Debug)]
struct PumpDrainProgress {
    request: PumpDrainRequest,
    start_seen: u64,
}

#[derive(Clone, Copy, Debug)]
struct PumpDrainReport {
    stats: PumpStats,
    drain_items: u64,
    remaining_queued: u64,
    outstanding_tasks: usize,
    stop_reason: PumpDrainStopReason,
}

impl PumpDrainReport {
    fn truncated(self) -> bool {
        self.remaining_queued > 0 || self.outstanding_tasks > 0
    }
}

struct PumpHandle {
    mode_tx: watch::Sender<PumpMode>,
    join: tokio::task::JoinHandle<Result<PumpDrainReport, OrchestratorError>>,
}

impl PumpHandle {
    async fn drain(
        self,
        log: &Arc<AnyEventLog>,
        topic_name: &str,
        up_to: u64,
        config: DrainConfig,
        overall_deadline: tokio::time::Instant,
    ) -> Result<PumpDrainReport, OrchestratorError> {
        let drain_deadline = std::cmp::min(
            tokio::time::Instant::now() + config.deadline,
            overall_deadline,
        );
        let _ = self.mode_tx.send(PumpMode::Draining(PumpDrainRequest {
            up_to,
            config,
            deadline: drain_deadline,
        }));
        append_pump_lifecycle_event(
            log,
            "pump_drain_started",
            json!({
                "topic": topic_name,
                "up_to": up_to,
                "max_items": config.max_items,
                "drain_deadline_ms": config.deadline.as_millis(),
            }),
        )
        .await?;
        match self.join.await {
            Ok(result) => result,
            Err(error) => Err(format!("pump task join failed: {error}").into()),
        }
    }
}

struct WaitpointSweepHandle {
    stop_tx: watch::Sender<bool>,
    join: tokio::task::JoinHandle<Result<(), OrchestratorError>>,
}

impl WaitpointSweepHandle {
    async fn shutdown(self) -> Result<(), OrchestratorError> {
        let _ = self.stop_tx.send(true);
        match self.join.await {
            Ok(result) => result,
            Err(error) => Err(format!("waitpoint sweeper join failed: {error}").into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PendingTriggerRecord {
    trigger_id: String,
    binding_version: u32,
    event: harn_vm::TriggerEvent,
}

// ── Pump spawn functions ──────────────────────────────────────────────────────

fn spawn_pending_pump(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    dispatcher: harn_vm::Dispatcher,
    pump_config: PumpConfig,
    metrics_registry: Arc<harn_vm::MetricsRegistry>,
    pump_drain_gate: PumpDrainGate,
    topic_name: &str,
) -> Result<PumpHandle, OrchestratorError> {
    let topic = harn_vm::event_log::Topic::new(topic_name).map_err(|error| error.to_string())?;
    spawn_topic_pump(
        event_log,
        topic,
        pump_config,
        metrics_registry,
        pump_drain_gate,
        move |logged| {
            let dispatcher = dispatcher.clone();
            async move {
                if pending_pump_test_should_fail() {
                    return Err("test pending pump failure".to_string().into());
                }
                if logged.kind != "trigger_event" {
                    return Ok(false);
                }
                let record: PendingTriggerRecord = serde_json::from_value(logged.payload)
                    .map_err(|error| format!("failed to decode pending trigger event: {error}"))?;
                dispatcher
                    .enqueue_targeted_with_headers(
                        Some(record.trigger_id),
                        Some(record.binding_version),
                        record.event,
                        Some(&logged.headers),
                    )
                    .await
                    .map_err(|error| format!("failed to enqueue pending trigger event: {error}"))?;
                Ok(true)
            }
        },
    )
}

fn spawn_cron_pump(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    dispatcher: harn_vm::Dispatcher,
    pump_config: PumpConfig,
    metrics_registry: Arc<harn_vm::MetricsRegistry>,
    pump_drain_gate: PumpDrainGate,
) -> Result<PumpHandle, OrchestratorError> {
    let topic =
        harn_vm::event_log::Topic::new(CRON_TICK_TOPIC).map_err(|error| error.to_string())?;
    spawn_topic_pump(
        event_log,
        topic,
        pump_config,
        metrics_registry,
        pump_drain_gate,
        move |logged| {
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
                    .enqueue_targeted_with_headers(trigger_id, None, event, Some(&logged.headers))
                    .await
                    .map_err(|error| format!("failed to enqueue cron trigger event: {error}"))?;
                Ok(true)
            }
        },
    )
}

fn spawn_inbox_pump(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    dispatcher: harn_vm::Dispatcher,
    pump_config: PumpConfig,
    metrics_registry: Arc<harn_vm::MetricsRegistry>,
    topic_name: &str,
) -> Result<PumpHandle, OrchestratorError> {
    let topic = harn_vm::event_log::Topic::new(topic_name).map_err(|error| error.to_string())?;
    let consumer = pump_consumer_id(&topic)?;
    let inbox_task_release_file = inbox_task_test_release_file();
    let (mode_tx, mut mode_rx) = watch::channel(PumpMode::Running);
    let join = tokio::task::spawn_local(async move {
        let start_from = event_log
            .consumer_cursor(&topic, &consumer)
            .await
            .map_err(|error| format!("failed to read consumer cursor for {topic}: {error}"))?
            .or(event_log
                .latest(&topic)
                .await
                .map_err(|error| format!("failed to read topic head {topic}: {error}"))?);
        let mut stream = event_log
            .clone()
            .subscribe(&topic, start_from)
            .await
            .map_err(|error| format!("failed to subscribe topic {topic}: {error}"))?;
        let mut stats = PumpStats {
            last_seen: start_from.unwrap_or(0),
            processed: 0,
        };
        record_pump_metrics(&metrics_registry, &event_log, &topic, stats.last_seen, 0).await?;
        let mut drain_progress = None;
        let mut tasks = JoinSet::new();

        loop {
            if let Some(progress) = drain_progress {
                if let Some(report) = maybe_finish_pump_drain(stats, progress, tasks.len()) {
                    return Ok(report);
                }
            }

            let deadline = drain_progress.map(|progress| progress.request.deadline);
            let mut deadline_wait = Box::pin(async move {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            });

            tokio::select! {
                changed = mode_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    if let PumpMode::Draining(request) = *mode_rx.borrow() {
                        drain_progress.get_or_insert(PumpDrainProgress {
                            request,
                            start_seen: stats.last_seen,
                        });
                    }
                }
                _ = &mut deadline_wait => {
                    if let Some(progress) = drain_progress {
                        return Ok(pump_drain_report(
                            stats,
                            progress.start_seen,
                            progress.request.up_to,
                            tasks.len(),
                            PumpDrainStopReason::Deadline,
                        ));
                    }
                }
                joined = tasks.join_next(), if !tasks.is_empty() => {
                    match joined {
                        Some(Ok(())) => {
                            record_pump_metrics(
                                &metrics_registry,
                                &event_log,
                                &topic,
                                stats.last_seen,
                                tasks.len(),
                            )
                            .await?;
                        }
                        Some(Err(error)) => {
                            return Err(format!("inbox dispatch task join failed: {error}").into());
                        }
                        None => {}
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(25)), if tasks.len() >= pump_config.max_outstanding => {
                    record_pump_metrics(&metrics_registry, &event_log, &topic, stats.last_seen, tasks.len()).await?;
                }
                received = stream.next(), if tasks.len() < pump_config.max_outstanding => {
                    let Some(received) = received else {
                        break;
                    };
                    let (event_id, logged) = received
                        .map_err(|error| format!("topic pump read failed for {topic}: {error}"))?;
                    if logged.kind != "event_ingested" {
                        stats.last_seen = event_id;
                        event_log
                            .ack(&topic, &consumer, event_id)
                            .await
                            .map_err(|error| format!("failed to ack topic pump cursor for {topic}: {error}"))?;
                        record_pump_metrics(&metrics_registry, &event_log, &topic, stats.last_seen, tasks.len()).await?;
                        continue;
                    }
                    append_pump_lifecycle_event(
                        &event_log,
                        "pump_received",
                        json!({
                            "topic": topic.as_str(),
                            "event_log_id": event_id,
                            "outstanding": tasks.len(),
                            "max_outstanding": pump_config.max_outstanding,
                        }),
                    )
                    .await?;
                    let envelope: harn_vm::triggers::dispatcher::InboxEnvelope =
                        serde_json::from_value(logged.payload)
                            .map_err(|error| format!("failed to decode dispatcher inbox event: {error}"))?;
                    let trigger_id = envelope.trigger_id.clone();
                    let binding_version = envelope.binding_version;
                    let trigger_event_id = envelope.event.id.0.clone();
                    let parent_headers = logged.headers.clone();
                    append_pump_lifecycle_event(
                        &event_log,
                        "pump_eligible",
                        json!({
                            "topic": topic.as_str(),
                            "event_log_id": event_id,
                            "trigger_id": trigger_id.clone(),
                            "binding_version": binding_version,
                            "trigger_event_id": trigger_event_id,
                        }),
                    )
                    .await?;
                    metrics_registry.record_orchestrator_pump_admission_delay(
                        topic.as_str(),
                        admission_delay(logged.occurred_at_ms),
                    );
                    append_pump_lifecycle_event(
                        &event_log,
                        "pump_admitted",
                        json!({
                            "topic": topic.as_str(),
                            "event_log_id": event_id,
                            "outstanding_after_admit": tasks.len() + 1,
                            "max_outstanding": pump_config.max_outstanding,
                            "trigger_id": trigger_id.clone(),
                            "binding_version": binding_version,
                            "trigger_event_id": trigger_event_id,
                        }),
                    )
                    .await?;
                    let dispatcher = dispatcher.clone();
                    let task_event_log = event_log.clone();
                    let task_topic = topic.as_str().to_string();
                    let inbox_task_release_file = inbox_task_release_file.clone();
                    tasks.spawn_local(async move {
                        if let Some(path) = inbox_task_release_file.as_ref() {
                            wait_for_test_release_file(path).await;
                        }
                        let _ = append_pump_lifecycle_event(
                            &task_event_log,
                            "pump_dispatch_started",
                            json!({
                                "topic": task_topic.clone(),
                                "event_log_id": event_id,
                                "trigger_id": trigger_id,
                                "binding_version": binding_version,
                                "trigger_event_id": trigger_event_id,
                            }),
                        )
                        .await;
                        let result = dispatcher
                            .dispatch_inbox_envelope_with_parent_headers(
                                envelope,
                                &parent_headers,
                            )
                            .await;
                        let (status, error_message) = match result {
                            Ok(_) => ("completed", None),
                            Err(error) => {
                                let message = error.to_string();
                                eprintln!("[harn] inbox dispatch warning: {message}");
                                ("failed", Some(message))
                            }
                        };
                        let _ = append_pump_lifecycle_event(
                            &task_event_log,
                            "pump_dispatch_completed",
                            json!({
                                "topic": task_topic,
                                "event_log_id": event_id,
                                "status": status,
                                "error": error_message,
                            }),
                        )
                        .await;
                    });
                    stats.last_seen = event_id;
                    stats.processed += 1;
                    event_log
                        .ack(&topic, &consumer, event_id)
                        .await
                        .map_err(|error| format!("failed to ack topic pump cursor for {topic}: {error}"))?;
                    append_pump_lifecycle_event(
                        &event_log,
                        "pump_acked",
                        json!({
                            "topic": topic.as_str(),
                            "event_log_id": event_id,
                            "cursor": event_id,
                        }),
                    )
                    .await?;
                    record_pump_metrics(&metrics_registry, &event_log, &topic, stats.last_seen, tasks.len()).await?;
                }
            }
        }

        while let Some(joined) = tasks.join_next().await {
            joined.map_err(|error| format!("inbox dispatch task join failed: {error}"))?;
            record_pump_metrics(
                &metrics_registry,
                &event_log,
                &topic,
                stats.last_seen,
                tasks.len(),
            )
            .await?;
        }

        Ok(drain_progress
            .map(|progress| {
                pump_drain_report(
                    stats,
                    progress.start_seen,
                    progress.request.up_to,
                    0,
                    PumpDrainStopReason::Drained,
                )
            })
            .unwrap_or_else(|| PumpDrainReport {
                stats,
                drain_items: 0,
                remaining_queued: 0,
                outstanding_tasks: 0,
                stop_reason: PumpDrainStopReason::Drained,
            }))
    });
    Ok(PumpHandle { mode_tx, join })
}

fn spawn_waitpoint_resume_pump(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    dispatcher: harn_vm::Dispatcher,
    pump_config: PumpConfig,
    metrics_registry: Arc<harn_vm::MetricsRegistry>,
    pump_drain_gate: PumpDrainGate,
) -> Result<PumpHandle, OrchestratorError> {
    let topic = harn_vm::event_log::Topic::new(harn_vm::WAITPOINT_RESUME_TOPIC)
        .map_err(|error| error.to_string())?;
    spawn_topic_pump(
        event_log,
        topic,
        pump_config,
        metrics_registry,
        pump_drain_gate,
        move |logged| {
            let dispatcher = dispatcher.clone();
            async move {
                harn_vm::process_waitpoint_resume_event(&dispatcher, logged)
                    .await
                    .map_err(OrchestratorError::from)
            }
        },
    )
}

fn spawn_waitpoint_cancel_pump(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    dispatcher: harn_vm::Dispatcher,
    pump_config: PumpConfig,
    metrics_registry: Arc<harn_vm::MetricsRegistry>,
    pump_drain_gate: PumpDrainGate,
) -> Result<PumpHandle, OrchestratorError> {
    let topic = harn_vm::event_log::Topic::new(harn_vm::TRIGGER_CANCEL_REQUESTS_TOPIC)
        .map_err(|error| error.to_string())?;
    spawn_topic_pump(
        event_log,
        topic,
        pump_config,
        metrics_registry,
        pump_drain_gate,
        move |logged| {
            let dispatcher = dispatcher.clone();
            async move {
                if logged.kind != "dispatch_cancel_requested" {
                    return Ok(false);
                }
                harn_vm::service_waitpoints_once(&dispatcher, None)
                    .await
                    .map_err(|error| {
                        OrchestratorError::Serve(format!(
                            "failed to service waitpoints on cancel: {error}"
                        ))
                    })?;
                Ok(true)
            }
        },
    )
}

fn spawn_waitpoint_sweeper(dispatcher: harn_vm::Dispatcher) -> WaitpointSweepHandle {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let join = tokio::task::spawn_local(async move {
        let mut interval = tokio::time::interval(WAITPOINT_SERVICE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    harn_vm::service_waitpoints_once(&dispatcher, None)
                        .await
                        .map_err(|error| format!("failed to service waitpoints on sweep: {error}"))?;
                }
            }
        }
        Ok(())
    });
    WaitpointSweepHandle { stop_tx, join }
}

fn spawn_topic_pump<F, Fut>(
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    topic: harn_vm::event_log::Topic,
    _pump_config: PumpConfig,
    metrics_registry: Arc<harn_vm::MetricsRegistry>,
    pump_drain_gate: PumpDrainGate,
    process: F,
) -> Result<PumpHandle, OrchestratorError>
where
    F: Fn(harn_vm::event_log::LogEvent) -> Fut + 'static,
    Fut: std::future::Future<Output = Result<bool, OrchestratorError>> + 'static,
{
    let consumer = pump_consumer_id(&topic)?;
    let mut pump_drain_gate_rx = pump_drain_gate.subscribe();
    let (mode_tx, mut mode_rx) = watch::channel(PumpMode::Running);
    let join = tokio::task::spawn_local(async move {
        let start_from = event_log
            .consumer_cursor(&topic, &consumer)
            .await
            .map_err(|error| format!("failed to read consumer cursor for {topic}: {error}"))?
            .or(event_log
                .latest(&topic)
                .await
                .map_err(|error| format!("failed to read topic head {topic}: {error}"))?);
        let mut stream = event_log
            .clone()
            .subscribe(&topic, start_from)
            .await
            .map_err(|error| format!("failed to subscribe topic {topic}: {error}"))?;
        let mut stats = PumpStats {
            last_seen: start_from.unwrap_or(0),
            processed: 0,
        };
        record_pump_metrics(&metrics_registry, &event_log, &topic, stats.last_seen, 0).await?;
        let mut drain_progress = None;
        loop {
            if let Some(progress) = drain_progress {
                if let Some(report) = maybe_finish_pump_drain(stats, progress, 0) {
                    return Ok(report);
                }
            }
            let deadline = drain_progress.map(|progress| progress.request.deadline);
            let mut deadline_wait = Box::pin(async move {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline).await;
                } else {
                    std::future::pending::<()>().await;
                }
            });
            tokio::select! {
                changed = mode_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    if let PumpMode::Draining(request) = *mode_rx.borrow() {
                        drain_progress.get_or_insert(PumpDrainProgress {
                            request,
                            start_seen: stats.last_seen,
                        });
                    }
                }
                _ = &mut deadline_wait => {
                    if let Some(progress) = drain_progress {
                        return Ok(pump_drain_report(
                            stats,
                            progress.start_seen,
                            progress.request.up_to,
                            0,
                            PumpDrainStopReason::Deadline,
                        ));
                    }
                }
                received = stream.next() => {
                    let Some(received) = received else {
                        break;
                    };
                    let (event_id, logged) = received
                        .map_err(|error| format!("topic pump read failed for {topic}: {error}"))?;
                    record_pump_metrics(&metrics_registry, &event_log, &topic, stats.last_seen, 1).await?;
                    metrics_registry.record_orchestrator_pump_admission_delay(
                        topic.as_str(),
                        admission_delay(logged.occurred_at_ms),
                    );
                    wait_for_pump_drain_release(
                        &event_log,
                        &topic,
                        event_id,
                        &mut pump_drain_gate_rx,
                    )
                    .await?;
                    let handled = process(logged).await?;
                    stats.last_seen = event_id;
                    if handled {
                        stats.processed += 1;
                    }
                    event_log
                        .ack(&topic, &consumer, event_id)
                        .await
                        .map_err(|error| format!("failed to ack topic pump cursor for {topic}: {error}"))?;
                    record_pump_metrics(&metrics_registry, &event_log, &topic, stats.last_seen, 0).await?;
                }
            }
        }
        Ok(drain_progress
            .map(|progress| {
                pump_drain_report(
                    stats,
                    progress.start_seen,
                    progress.request.up_to,
                    0,
                    PumpDrainStopReason::Drained,
                )
            })
            .unwrap_or_else(|| PumpDrainReport {
                stats,
                drain_items: 0,
                remaining_queued: 0,
                outstanding_tasks: 0,
                stop_reason: PumpDrainStopReason::Drained,
            }))
    });
    Ok(PumpHandle { mode_tx, join })
}

// ── Graceful shutdown ─────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn graceful_shutdown(
    ctx: GracefulShutdownCtx<'_>,
    listener: ListenerRuntime,
    dispatcher: harn_vm::Dispatcher,
    pending_pumps: Vec<(String, PumpHandle)>,
    cron_pump: PumpHandle,
    inbox_pumps: Vec<(String, PumpHandle)>,
    waitpoint_pump: PumpHandle,
    waitpoint_cancel_pump: PumpHandle,
    waitpoint_sweeper: WaitpointSweepHandle,
) -> Result<(), OrchestratorError> {
    eprintln!("[harn] signal received, starting graceful shutdown...");
    tracing::info!(
        component = "orchestrator",
        trace_id = "",
        shutdown_timeout_secs = ctx.shutdown_timeout.as_secs(),
        "signal received, starting graceful shutdown"
    );
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
            "drain_max_items": ctx.drain_config.max_items,
            "drain_deadline_secs": ctx.drain_config.deadline.as_secs(),
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

    let mut pending_processed = 0;
    for (topic_name, pump) in pending_pumps {
        let stats =
            drain_pump_best_effort(ctx.event_log, &topic_name, pump, ctx.drain_config, deadline)
                .await?;
        pending_processed += stats.stats.processed;
        emit_drain_truncated(ctx.event_log, &topic_name, stats, ctx.drain_config).await?;
    }
    let cron_stats = drain_pump_best_effort(
        ctx.event_log,
        CRON_TICK_TOPIC,
        cron_pump,
        ctx.drain_config,
        deadline,
    )
    .await?;
    emit_drain_truncated(ctx.event_log, CRON_TICK_TOPIC, cron_stats, ctx.drain_config).await?;
    let mut inbox_processed = 0;
    for (topic_name, pump) in inbox_pumps {
        let stats =
            drain_pump_best_effort(ctx.event_log, &topic_name, pump, ctx.drain_config, deadline)
                .await?;
        inbox_processed += stats.stats.processed;
        emit_drain_truncated(ctx.event_log, &topic_name, stats, ctx.drain_config).await?;
    }
    let waitpoint_stats = waitpoint_pump
        .drain(
            ctx.event_log,
            harn_vm::WAITPOINT_RESUME_TOPIC,
            topic_latest_id(ctx.event_log, harn_vm::WAITPOINT_RESUME_TOPIC).await?,
            ctx.drain_config,
            deadline,
        )
        .await?;
    emit_drain_truncated(
        ctx.event_log,
        harn_vm::WAITPOINT_RESUME_TOPIC,
        waitpoint_stats,
        ctx.drain_config,
    )
    .await?;
    let waitpoint_cancel_stats = waitpoint_cancel_pump
        .drain(
            ctx.event_log,
            harn_vm::TRIGGER_CANCEL_REQUESTS_TOPIC,
            topic_latest_id(ctx.event_log, harn_vm::TRIGGER_CANCEL_REQUESTS_TOPIC).await?,
            ctx.drain_config,
            deadline,
        )
        .await?;
    emit_drain_truncated(
        ctx.event_log,
        harn_vm::TRIGGER_CANCEL_REQUESTS_TOPIC,
        waitpoint_cancel_stats,
        ctx.drain_config,
    )
    .await?;
    waitpoint_sweeper.shutdown().await?;
    let drain_report = dispatcher
        .drain(remaining_budget(deadline))
        .await
        .map_err(|error| format!("failed to drain dispatcher: {error}"))?;

    let stopped_at = now_rfc3339()?;
    let timed_out = !drain_report.drained;
    if timed_out {
        dispatcher.shutdown();
    }
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
            "pending_events_drained": pending_processed,
            "cron_events_drained": cron_stats.stats.processed,
            "inbox_events_drained": inbox_processed,
            "waitpoint_events_drained": waitpoint_stats.stats.processed,
            "waitpoint_cancel_events_drained": waitpoint_cancel_stats.stats.processed,
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
            listener_url: ctx.listener_url.clone(),
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
    tracing::info!(
        component = "orchestrator",
        trace_id = "",
        "graceful shutdown complete"
    );
    Ok(())
}

struct GracefulShutdownCtx<'a> {
    role: OrchestratorRole,
    bind: SocketAddr,
    listener_url: String,
    config_path: &'a Path,
    state_dir: &'a Path,
    startup_started_at: &'a str,
    event_log: &'a Arc<AnyEventLog>,
    event_log_description: &'a harn_vm::event_log::EventLogDescription,
    secret_chain_display: &'a str,
    triggers: &'a [CollectedManifestTrigger],
    connectors: &'a ConnectorRuntime,
    shutdown_timeout: Duration,
    drain_config: DrainConfig,
}

// ── Manifest reload ───────────────────────────────────────────────────────────

struct RuntimeCtx<'a> {
    role: OrchestratorRole,
    config_path: &'a Path,
    state_dir: &'a Path,
    bind: SocketAddr,
    startup_started_at: &'a str,
    event_log: &'a Arc<AnyEventLog>,
    event_log_description: &'a harn_vm::event_log::EventLogDescription,
    secret_chain_display: &'a str,
    listener: &'a ListenerRuntime,
    connectors: &'a mut ConnectorRuntime,
    live_manifest: &'a mut Manifest,
    live_triggers: &'a mut Vec<CollectedManifestTrigger>,
    secret_provider: &'a Arc<dyn harn_vm::secrets::SecretProvider>,
    metrics_registry: &'a Arc<harn_vm::MetricsRegistry>,
    mcp_service: Option<&'a Arc<crate::commands::mcp::serve::McpOrchestratorService>>,
    #[cfg_attr(not(unix), allow(dead_code))]
    reload_rx: &'a mut mpsc::UnboundedReceiver<AdminReloadRequest>,
}

#[cfg_attr(not(unix), allow(dead_code))]
async fn handle_reload_request(
    ctx: &mut RuntimeCtx<'_>,
    request: AdminReloadRequest,
) -> Result<(), OrchestratorError> {
    let source = request.source.clone();
    match reload_manifest(ctx).await {
        Ok(summary) => {
            if let Some(mcp_service) = ctx.mcp_service {
                mcp_service.notify_manifest_reloaded();
            }
            write_running_state_snapshot(ctx)?;
            append_manifest_event(
                ctx.event_log,
                "reload_succeeded",
                json!({
                    "source": source,
                    "summary": summary,
                }),
            )
            .await?;
            eprintln!(
                "[harn] manifest reload ({source}) applied: +{} ~{} -{}",
                summary.added.len(),
                summary.modified.len(),
                summary.removed.len()
            );
            if let Some(response_tx) = request.response_tx {
                let _ = response_tx.send(serde_json::to_value(&summary).map_err(|error| {
                    OrchestratorError::Serve(format!("failed to encode reload summary: {error}"))
                }));
            }
        }
        Err(error) => {
            eprintln!("[harn] manifest reload ({source}) failed: {error}");
            append_manifest_event(
                ctx.event_log,
                "reload_failed",
                json!({
                    "source": source,
                    "error": error.to_string(),
                }),
            )
            .await?;
            if let Some(response_tx) = request.response_tx {
                let _ = response_tx.send(Err(error));
            }
        }
    }
    Ok(())
}

#[cfg_attr(not(unix), allow(dead_code))]
fn write_running_state_snapshot(ctx: &RuntimeCtx<'_>) -> Result<(), OrchestratorError> {
    let listener_metrics = ctx.listener.trigger_metrics();
    write_state_snapshot(
        &ctx.state_dir.join(STATE_SNAPSHOT_FILE),
        &ServeStateSnapshot {
            status: "running".to_string(),
            role: ctx.role.as_str().to_string(),
            bind: ctx.bind.to_string(),
            listener_url: ctx.listener.url(),
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
    )
}

#[cfg_attr(not(unix), allow(dead_code))]
async fn reload_manifest(
    ctx: &mut RuntimeCtx<'_>,
) -> Result<ManifestReloadSummary, OrchestratorError> {
    let (manifest, manifest_dir) = load_manifest(ctx.config_path)?;
    let mut vm = ctx
        .role
        .build_vm(&manifest_dir, &manifest_dir, ctx.state_dir)?;
    let extensions = package::load_runtime_extensions(ctx.config_path);
    let collected_triggers = package::collect_manifest_triggers(&mut vm, &extensions)
        .await
        .map_err(|error| format!("failed to collect manifest triggers: {error}"))?;
    let summary = summarize_manifest_reload(ctx.live_triggers, &collected_triggers);
    let connector_reload =
        connector_reload_fingerprint_map(ctx.live_triggers, &ctx.connectors.provider_overrides)
            != connector_reload_fingerprint_map(
                &collected_triggers,
                &extensions.provider_connectors,
            );
    let next_connector_runtime = if connector_reload {
        let mut runtime = initialize_connectors(
            &collected_triggers,
            ctx.event_log.clone(),
            ctx.secret_provider.clone(),
            ctx.metrics_registry.clone(),
            &extensions.provider_connectors,
        )
        .await?;
        runtime.activations = runtime
            .registry
            .activate_all(&runtime.trigger_registry)
            .await
            .map_err(|error| error.to_string())?;
        Some(runtime)
    } else {
        None
    };
    let previous_manifest = ctx.live_manifest.clone();
    let previous_triggers = ctx.live_triggers.clone();
    package::install_collected_manifest_triggers(&collected_triggers).await?;
    apply_supervisor_state(ctx.state_dir).await?;
    let binding_versions = live_manifest_binding_versions();
    let route_registry = next_connector_runtime
        .as_ref()
        .map(|runtime| &runtime.registry)
        .unwrap_or(&ctx.connectors.registry);
    let route_overrides = next_connector_runtime
        .as_ref()
        .map(|runtime| runtime.provider_overrides.as_slice())
        .unwrap_or(ctx.connectors.provider_overrides.as_slice());
    let route_configs = match build_route_configs(&collected_triggers, &binding_versions)
        .and_then(|routes| attach_route_connectors(routes, route_registry, route_overrides))
    {
        Ok(routes) => routes,
        Err(error) => {
            rollback_manifest_reload(ctx, &previous_manifest, &previous_triggers)
                .await
                .map_err(|rollback| format!("{error}; rollback failed: {rollback}"))?;
            return Err(error);
        }
    };
    if let Err(error) = ctx.listener.reload_routes(route_configs) {
        rollback_manifest_reload(ctx, &previous_manifest, &previous_triggers)
            .await
            .map_err(|rollback| format!("{error}; rollback failed: {rollback}"))?;
        return Err(error);
    }
    if let Some(runtime) = next_connector_runtime {
        let previous_handles = ctx.connectors.handles.clone();
        let connector_clients = runtime.registry.client_map().await;
        harn_vm::install_active_connector_clients(connector_clients);
        *ctx.connectors = runtime;
        for handle in previous_handles {
            let connector = handle.lock().await;
            if let Err(error) = connector.shutdown(Duration::from_secs(5)).await {
                eprintln!(
                    "[harn] connector {} reload shutdown warning: {error}",
                    connector.provider_id().as_str()
                );
            }
        }
    }
    *ctx.live_manifest = manifest;
    *ctx.live_triggers = collected_triggers;
    Ok(summary)
}

#[cfg_attr(not(unix), allow(dead_code))]
async fn rollback_manifest_reload(
    ctx: &mut RuntimeCtx<'_>,
    previous_manifest: &Manifest,
    previous_triggers: &[CollectedManifestTrigger],
) -> Result<(), OrchestratorError> {
    package::install_collected_manifest_triggers(previous_triggers).await?;
    apply_supervisor_state(ctx.state_dir).await?;
    let binding_versions = live_manifest_binding_versions();
    let route_configs = build_route_configs(previous_triggers, &binding_versions)?;
    let route_configs = attach_route_connectors(
        route_configs,
        &ctx.connectors.registry,
        &ctx.connectors.provider_overrides,
    )?;
    ctx.listener.reload_routes(route_configs)?;
    *ctx.live_manifest = previous_manifest.clone();
    *ctx.live_triggers = previous_triggers.to_vec();
    Ok(())
}

#[cfg_attr(not(unix), allow(dead_code))]
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

#[cfg_attr(not(unix), allow(dead_code))]
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

#[cfg_attr(not(unix), allow(dead_code))]
fn connector_reload_fingerprint_map(
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

async fn initialize_connectors(
    triggers: &[CollectedManifestTrigger],
    event_log: Arc<harn_vm::event_log::AnyEventLog>,
    secrets: Arc<dyn harn_vm::secrets::SecretProvider>,
    metrics: Arc<harn_vm::MetricsRegistry>,
    provider_overrides: &[ResolvedProviderConnectorConfig],
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
) -> Box<dyn harn_vm::Connector> {
    match provider.as_str() {
        "cron" => Box::new(harn_vm::CronConnector::new()),
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

fn build_route_configs(
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

fn attach_route_connectors(
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

fn connector_owns_ingress(
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

// ── Misc helpers ──────────────────────────────────────────────────────────────

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
        CollectedTriggerHandler::Persona { .. } => "persona",
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

fn has_orchestrator_api_keys_configured() -> bool {
    std::env::var("HARN_ORCHESTRATOR_API_KEYS")
        .ok()
        .is_some_and(|value| value.split(',').any(|segment| !segment.trim().is_empty()))
}

fn has_mcp_oauth_configured() -> bool {
    std::env::var("HARN_MCP_OAUTH_AUTHORIZATION_SERVERS")
        .ok()
        .is_some_and(|value| value.split(',').any(|segment| !segment.trim().is_empty()))
}

fn validate_mcp_paths(
    path: &str,
    sse_path: &str,
    messages_path: &str,
) -> Result<(), OrchestratorError> {
    let reserved = [
        "/health",
        "/healthz",
        "/readyz",
        "/metrics",
        "/admin/reload",
        "/acp",
    ];
    let mut seen = BTreeSet::new();
    for (label, value) in [
        ("--mcp-path", path),
        ("--mcp-sse-path", sse_path),
        ("--mcp-messages-path", messages_path),
    ] {
        if !value.starts_with('/') {
            return Err(format!("{label} must start with '/'").into());
        }
        if value == "/" {
            return Err(format!("{label} cannot be '/'").into());
        }
        if reserved.contains(&value) {
            return Err(format!("{label} cannot use reserved listener path '{value}'").into());
        }
        if !seen.insert(value) {
            return Err(format!("embedded MCP paths must be unique; duplicate '{value}'").into());
        }
    }
    Ok(())
}

async fn append_lifecycle_event(
    log: &Arc<AnyEventLog>,
    kind: &str,
    payload: JsonValue,
) -> Result<(), OrchestratorError> {
    let topic =
        harn_vm::event_log::Topic::new(LIFECYCLE_TOPIC).map_err(|error| error.to_string())?;
    log.append(&topic, harn_vm::event_log::LogEvent::new(kind, payload))
        .await
        .map(|_| ())
        .map_err(|error| {
            OrchestratorError::Serve(format!(
                "failed to append orchestrator lifecycle event: {error}"
            ))
        })
}

async fn append_pump_lifecycle_event(
    log: &Arc<AnyEventLog>,
    kind: &str,
    payload: JsonValue,
) -> Result<(), OrchestratorError> {
    append_lifecycle_event(log, kind, payload).await
}

async fn wait_for_pump_drain_release(
    log: &Arc<AnyEventLog>,
    topic: &harn_vm::event_log::Topic,
    event_id: u64,
    gate_rx: &mut watch::Receiver<bool>,
) -> Result<(), OrchestratorError> {
    if !*gate_rx.borrow() {
        return Ok(());
    }
    append_pump_lifecycle_event(
        log,
        "pump_drain_waiting",
        json!({
            "topic": topic.as_str(),
            "event_log_id": event_id,
        }),
    )
    .await?;
    while *gate_rx.borrow() {
        if gate_rx.changed().await.is_err() {
            break;
        }
    }
    Ok(())
}

#[cfg_attr(not(unix), allow(dead_code))]
async fn append_manifest_event(
    log: &Arc<AnyEventLog>,
    kind: &str,
    payload: JsonValue,
) -> Result<(), OrchestratorError> {
    let topic =
        harn_vm::event_log::Topic::new(MANIFEST_TOPIC).map_err(|error| error.to_string())?;
    log.append(&topic, harn_vm::event_log::LogEvent::new(kind, payload))
        .await
        .map(|_| ())
        .map_err(|error| {
            OrchestratorError::Serve(format!(
                "failed to append orchestrator manifest event: {error}"
            ))
        })
}

async fn record_pump_metrics(
    metrics: &harn_vm::MetricsRegistry,
    log: &Arc<AnyEventLog>,
    topic: &harn_vm::event_log::Topic,
    last_seen: u64,
    outstanding: usize,
) -> Result<(), OrchestratorError> {
    let latest = log.latest(topic).await.ok().flatten().unwrap_or(last_seen);
    let backlog = latest.saturating_sub(last_seen);
    metrics.set_orchestrator_pump_outstanding(topic.as_str(), outstanding);
    metrics.set_orchestrator_pump_backlog(topic.as_str(), backlog);
    append_pump_lifecycle_event(
        log,
        "pump_metrics_recorded",
        json!({
            "topic": topic.as_str(),
            "latest": latest,
            "last_seen": last_seen,
            "backlog": backlog,
            "outstanding": outstanding,
        }),
    )
    .await
}

fn admission_delay(occurred_at_ms: i64) -> Duration {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    Duration::from_millis(now.saturating_sub(occurred_at_ms).max(0) as u64)
}

async fn emit_drain_truncated(
    log: &Arc<AnyEventLog>,
    topic_name: &str,
    report: PumpDrainReport,
    config: DrainConfig,
) -> Result<(), OrchestratorError> {
    if !report.truncated() {
        return Ok(());
    }
    eprintln!(
        "[harn] warning: pump drain truncated for {topic_name}: remaining_queued={} drain_items={} reason={}",
        report.remaining_queued,
        report.drain_items,
        report.stop_reason.as_str()
    );
    append_lifecycle_event(
        log,
        "drain_truncated",
        json!({
            "topic": topic_name,
            "remaining_queued": report.remaining_queued,
            "drain_items": report.drain_items,
            "outstanding_tasks": report.outstanding_tasks,
            "max_items": config.max_items,
            "deadline_secs": config.deadline.as_secs(),
            "reason": report.stop_reason.as_str(),
        }),
    )
    .await
}

async fn topic_latest_id(
    log: &Arc<AnyEventLog>,
    topic_name: &str,
) -> Result<u64, OrchestratorError> {
    let topic = harn_vm::event_log::Topic::new(topic_name).map_err(|error| error.to_string())?;
    log.latest(&topic)
        .await
        .map(|value| value.unwrap_or(0))
        .map_err(|error| {
            OrchestratorError::Serve(format!(
                "failed to read topic head for {topic_name}: {error}"
            ))
        })
}

async fn drain_pump_best_effort(
    log: &Arc<AnyEventLog>,
    topic_name: &str,
    pump: PumpHandle,
    config: DrainConfig,
    overall_deadline: tokio::time::Instant,
) -> Result<PumpDrainReport, OrchestratorError> {
    let topic = harn_vm::event_log::Topic::new(topic_name).map_err(|error| error.to_string())?;
    let consumer = pump_consumer_id(&topic)?;
    let start_seen = log
        .consumer_cursor(&topic, &consumer)
        .await
        .map_err(|error| format!("failed to read consumer cursor for {topic_name}: {error}"))?
        .unwrap_or(0);
    let up_to = log
        .latest(&topic)
        .await
        .map_err(|error| format!("failed to read topic head for {topic_name}: {error}"))?
        .unwrap_or(0);
    let budget = remaining_budget(overall_deadline);

    match tokio::time::timeout(
        budget,
        pump.drain(log, topic_name, up_to, config, overall_deadline),
    )
    .await
    {
        Ok(Ok(report)) => Ok(report),
        Ok(Err(error)) => {
            eprintln!("[harn] warning: pump drain error for {topic_name}: {error}");
            best_effort_pump_report(
                log,
                &topic,
                &consumer,
                start_seen,
                up_to,
                PumpDrainStopReason::Error,
            )
            .await
        }
        Err(_) => {
            eprintln!(
                "[harn] warning: pump drain timed out for {topic_name} after {:?}",
                budget
            );
            best_effort_pump_report(
                log,
                &topic,
                &consumer,
                start_seen,
                up_to,
                PumpDrainStopReason::Deadline,
            )
            .await
        }
    }
}

async fn best_effort_pump_report(
    log: &Arc<AnyEventLog>,
    topic: &harn_vm::event_log::Topic,
    consumer: &ConsumerId,
    start_seen: u64,
    up_to: u64,
    stop_reason: PumpDrainStopReason,
) -> Result<PumpDrainReport, OrchestratorError> {
    let last_seen = log
        .consumer_cursor(topic, consumer)
        .await
        .map_err(|error| format!("failed to read consumer cursor for {topic}: {error}"))?
        .unwrap_or(start_seen);
    let stats = PumpStats {
        last_seen,
        processed: last_seen.saturating_sub(start_seen),
    };
    Ok(pump_drain_report(stats, start_seen, up_to, 0, stop_reason))
}

fn remaining_budget(deadline: tokio::time::Instant) -> Duration {
    deadline.saturating_duration_since(tokio::time::Instant::now())
}

fn maybe_finish_pump_drain(
    stats: PumpStats,
    progress: PumpDrainProgress,
    outstanding_tasks: usize,
) -> Option<PumpDrainReport> {
    if stats.last_seen >= progress.request.up_to && outstanding_tasks == 0 {
        return Some(pump_drain_report(
            stats,
            progress.start_seen,
            progress.request.up_to,
            outstanding_tasks,
            PumpDrainStopReason::Drained,
        ));
    }
    if outstanding_tasks > 0 {
        if tokio::time::Instant::now() >= progress.request.deadline {
            return Some(pump_drain_report(
                stats,
                progress.start_seen,
                progress.request.up_to,
                outstanding_tasks,
                PumpDrainStopReason::Deadline,
            ));
        }
        return None;
    }
    let drain_items = stats.last_seen.saturating_sub(progress.start_seen);
    if drain_items >= progress.request.config.max_items as u64 {
        return Some(pump_drain_report(
            stats,
            progress.start_seen,
            progress.request.up_to,
            outstanding_tasks,
            PumpDrainStopReason::MaxItems,
        ));
    }
    if tokio::time::Instant::now() >= progress.request.deadline {
        return Some(pump_drain_report(
            stats,
            progress.start_seen,
            progress.request.up_to,
            outstanding_tasks,
            PumpDrainStopReason::Deadline,
        ));
    }
    None
}

fn pump_drain_report(
    stats: PumpStats,
    start_seen: u64,
    up_to: u64,
    outstanding_tasks: usize,
    stop_reason: PumpDrainStopReason,
) -> PumpDrainReport {
    PumpDrainReport {
        stats,
        drain_items: stats.last_seen.saturating_sub(start_seen),
        remaining_queued: up_to.saturating_sub(stats.last_seen),
        outstanding_tasks,
        stop_reason,
    }
}

fn pump_consumer_id(topic: &harn_vm::event_log::Topic) -> Result<ConsumerId, OrchestratorError> {
    ConsumerId::new(format!("orchestrator-pump.{}", topic.as_str())).map_err(|error| {
        OrchestratorError::Serve(format!("failed to create consumer id for {topic}: {error}"))
    })
}

fn inbox_task_test_release_file() -> Option<PathBuf> {
    test_file_from_env(TEST_INBOX_TASK_RELEASE_FILE_ENV)
}

fn test_file_from_env(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

async fn wait_for_test_release_file(path: &Path) {
    while tokio::fs::metadata(path).await.is_err() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn pending_pump_test_should_fail() -> bool {
    std::env::var(TEST_FAIL_PENDING_PUMP_ENV)
        .ok()
        .is_some_and(|value| value != "0")
}

fn spawn_manifest_watcher(
    config_path: PathBuf,
    reload: AdminReloadHandle,
) -> Result<notify::RecommendedWatcher, OrchestratorError> {
    use notify::{Event, EventKind, RecursiveMode, Watcher};

    let watch_dir = config_path.parent().ok_or_else(|| {
        format!(
            "manifest has no parent directory: {}",
            config_path.display()
        )
    })?;
    let target_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            format!(
                "manifest path is not valid UTF-8: {}",
                config_path.display()
            )
        })?
        .to_string();
    let (tx, mut rx) = mpsc::unbounded_channel::<()>();
    tokio::task::spawn_local(async move {
        while rx.recv().await.is_some() {
            tokio::time::sleep(Duration::from_millis(200)).await;
            while rx.try_recv().is_ok() {}
            let _ = reload.trigger("file_watch");
        }
    });
    let mut watcher =
        notify::recommended_watcher(move |res: Result<Event, notify::Error>| match res {
            Ok(event)
                if matches!(
                    event.kind,
                    EventKind::Modify(_)
                        | EventKind::Create(_)
                        | EventKind::Remove(_)
                        | EventKind::Any
                ) && event.paths.iter().any(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name == target_name)
                }) =>
            {
                let _ = tx.send(());
            }
            _ => {}
        })
        .map_err(|error| format!("failed to create manifest watcher: {error}"))?;
    watcher
        .watch(watch_dir, RecursiveMode::NonRecursive)
        .map_err(|error| {
            format!(
                "failed to watch manifest directory {}: {error}",
                watch_dir.display()
            )
        })?;
    Ok(watcher)
}

pub(crate) fn load_manifest(config_path: &Path) -> Result<(Manifest, PathBuf), OrchestratorError> {
    if !config_path.is_file() {
        return Err(format!("manifest not found: {}", config_path.display()).into());
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

pub(crate) fn absolutize_from_cwd(path: &Path) -> Result<PathBuf, OrchestratorError> {
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

fn now_rfc3339() -> Result<String, OrchestratorError> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| OrchestratorError::Serve(format!("failed to format timestamp: {error}")))
}

fn write_state_snapshot(
    path: &Path,
    snapshot: &ServeStateSnapshot,
) -> Result<(), OrchestratorError> {
    let encoded = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| format!("failed to encode orchestrator state snapshot: {error}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    std::fs::write(path, encoded).map_err(|error| {
        OrchestratorError::Serve(format!("failed to write {}: {error}", path.display()))
    })
}

// ── Snapshot types ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct ServeStateSnapshot {
    status: String,
    role: String,
    bind: String,
    listener_url: String,
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use harn_vm::event_log::{EventLog, Topic};

    fn write_test_file(dir: &Path, relative: &str, contents: &str) {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn stream_manifest_fixture() -> &'static str {
        r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "ws-stream"
kind = "stream"
provider = "websocket"
path = "/streams/ws"
match = { events = ["quote.tick"] }
handler = "handlers::on_stream"
"#
    }

    fn stream_handler_fixture(marker_path: &Path) -> String {
        format!(
            r#"
import "std/triggers"

pub fn on_stream(event: TriggerEvent) {{
  write_file({marker:?}, json_stringify({{
    provider: event.provider,
    kind: event.kind,
    key: event.provider_payload.key,
    stream: event.provider_payload.stream,
    amount: event.provider_payload.raw.value.amount,
  }}))
}}
"#,
            marker = marker_path.display().to_string()
        )
    }

    /// Proof test: `stream_trigger_route_uses_generic_stream_connector`
    /// migrated from subprocess + SQLite polling to in-process harness +
    /// `EventLog::subscribe()`.
    #[tokio::test(flavor = "multi_thread")]
    async fn stream_trigger_route_uses_generic_stream_connector_in_process() {
        // Env vars are process-global; hold the lock for the entire test so
        // concurrent unit tests that also set env vars don't race.
        let _env_lock = crate::tests::common::env_lock::lock_env().lock().await;
        let _secret_providers = crate::env_guard::ScopedEnvVar::set("HARN_SECRET_PROVIDERS", "env");

        let temp = tempfile::TempDir::new().unwrap();
        let marker_path = temp.path().join("stream-handler.json");
        write_test_file(temp.path(), "harn.toml", stream_manifest_fixture());
        write_test_file(
            temp.path(),
            "lib.harn",
            &stream_handler_fixture(&marker_path),
        );

        let config =
            OrchestratorConfig::for_test(temp.path().join("harn.toml"), temp.path().join("state"));
        let harness = OrchestratorHarness::start(config)
            .await
            .expect("harness start");
        let base_url = harness.listener_url().to_string();
        let event_log = harness.event_log();

        let response = reqwest::Client::new()
            .post(format!("{base_url}/streams/ws"))
            .header("content-type", "application/json")
            .json(&serde_json::json!({
                "key": "acct-1",
                "stream": "quotes",
                "value": {"amount": 10}
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);

        // Event-driven wait: subscribe to orchestrator.lifecycle and block
        // until pump_dispatch_completed arrives, replacing SQLite polling.
        let topic = Topic::new("orchestrator.lifecycle").unwrap();
        let mut stream = event_log.clone().subscribe(&topic, None).await.unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let remaining = deadline
                .checked_duration_since(tokio::time::Instant::now())
                .expect("timed out waiting for pump_dispatch_completed");
            let (_, event) = tokio::time::timeout(remaining, stream.next())
                .await
                .expect("timed out waiting for pump_dispatch_completed event")
                .expect("event stream ended unexpectedly")
                .expect("event stream error");
            if event.kind == "pump_dispatch_completed"
                && event.payload["status"] == serde_json::json!("completed")
            {
                break;
            }
        }
        drop(stream);

        let marker: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&marker_path).unwrap()).unwrap();
        assert_eq!(
            marker.get("provider").and_then(|v| v.as_str()),
            Some("websocket")
        );
        assert_eq!(
            marker.get("kind").and_then(|v| v.as_str()),
            Some("quote.tick")
        );
        assert_eq!(marker.get("key").and_then(|v| v.as_str()), Some("acct-1"));
        assert_eq!(
            marker.get("stream").and_then(|v| v.as_str()),
            Some("quotes")
        );
        assert_eq!(marker.get("amount").and_then(|v| v.as_i64()), Some(10));

        harness
            .shutdown(std::time::Duration::from_secs(5))
            .await
            .expect("harness shutdown");
    }
}
