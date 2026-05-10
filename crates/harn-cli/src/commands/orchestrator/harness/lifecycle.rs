//! Orchestrator startup, runtime loop, and manifest watcher.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::{mpsc, oneshot, watch};

use harn_vm::event_log::EventLog;

use super::super::common::stranded_envelopes;
use super::super::errors::OrchestratorError;
use super::super::listener::{AdminReloadHandle, ListenerConfig, ListenerRuntime};
use super::super::origin_guard::OriginAllowList;
use super::super::role::OrchestratorRole;
use super::super::supervisor_state::apply_supervisor_state;
use super::audit::{
    append_lifecycle_event, now_rfc3339, validate_mcp_paths, write_state_snapshot,
    ConnectorActivationSnapshot, ServeStateSnapshot,
};
use super::config::{DrainConfig, OrchestratorConfig, PumpConfig};
use super::pumps::{
    spawn_cron_pump, spawn_inbox_pump, spawn_pending_pump, spawn_waitpoint_cancel_pump,
    spawn_waitpoint_resume_pump, spawn_waitpoint_sweeper, PumpDrainGate,
};
use super::reload::{handle_reload_request, RuntimeCtx};
use super::routing::{
    attach_route_connectors, build_route_configs, format_activation_summary,
    format_trigger_summary, initialize_connectors, live_manifest_binding_versions,
    trigger_state_snapshots,
};
use super::shutdown::{graceful_shutdown, GracefulShutdownCtx};
use super::ReadyState;
use super::{PENDING_TOPIC, STATE_SNAPSHOT_FILE};
use crate::package::{self, Manifest};

pub(super) async fn orchestrator_task(
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
        config.clock.clone(),
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
        clock: config.clock.clone(),
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
