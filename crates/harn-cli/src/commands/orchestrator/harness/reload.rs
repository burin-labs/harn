//! Manifest hot-reload: apply, rollback, and emit lifecycle events.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use harn_vm::clock::Clock;
use serde_json::json;
use tokio::sync::mpsc;

use harn_vm::event_log::AnyEventLog;

use super::super::errors::OrchestratorError;
use super::super::listener::{AdminReloadRequest, ListenerRuntime};
use super::super::role::OrchestratorRole;
use super::super::supervisor_state::apply_supervisor_state;
use super::audit::{
    append_manifest_event, write_state_snapshot, ConnectorActivationSnapshot, ServeStateSnapshot,
};
use super::routing::{
    attach_route_connectors, build_route_configs, connector_reload_fingerprint_map,
    initialize_connectors, live_manifest_binding_versions, summarize_manifest_reload,
    trigger_state_snapshots, ConnectorRuntime, ManifestReloadSummary,
};
use super::STATE_SNAPSHOT_FILE;
use crate::package::{self, CollectedManifestTrigger, Manifest};

pub(super) struct RuntimeCtx<'a> {
    pub(super) role: OrchestratorRole,
    pub(super) config_path: &'a Path,
    pub(super) state_dir: &'a Path,
    pub(super) bind: SocketAddr,
    pub(super) startup_started_at: &'a str,
    pub(super) event_log: &'a Arc<AnyEventLog>,
    pub(super) event_log_description: &'a harn_vm::event_log::EventLogDescription,
    pub(super) secret_chain_display: &'a str,
    pub(super) listener: &'a ListenerRuntime,
    pub(super) connectors: &'a mut ConnectorRuntime,
    pub(super) live_manifest: &'a mut Manifest,
    pub(super) live_triggers: &'a mut Vec<CollectedManifestTrigger>,
    pub(super) secret_provider: &'a Arc<dyn harn_vm::secrets::SecretProvider>,
    pub(super) metrics_registry: &'a Arc<harn_vm::MetricsRegistry>,
    pub(super) mcp_service: Option<&'a Arc<crate::commands::mcp::serve::McpOrchestratorService>>,
    pub(super) clock: Arc<dyn Clock>,
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(super) reload_rx: &'a mut mpsc::UnboundedReceiver<AdminReloadRequest>,
}

#[cfg_attr(not(unix), allow(dead_code))]
pub(super) async fn handle_reload_request(
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
pub(super) fn write_running_state_snapshot(ctx: &RuntimeCtx<'_>) -> Result<(), OrchestratorError> {
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
pub(super) async fn reload_manifest(
    ctx: &mut RuntimeCtx<'_>,
) -> Result<ManifestReloadSummary, OrchestratorError> {
    let (manifest, manifest_dir) = super::load_manifest(ctx.config_path)?;
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
            ctx.clock.clone(),
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
