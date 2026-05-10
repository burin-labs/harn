//! Graceful shutdown of the listener, connectors, pumps, and dispatcher.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use harn_vm::event_log::{AnyEventLog, EventLog};

use super::super::errors::OrchestratorError;
use super::super::listener::ListenerRuntime;
use super::super::role::OrchestratorRole;
use super::audit::{
    append_lifecycle_event, now_rfc3339, write_state_snapshot, ConnectorActivationSnapshot,
    ServeStateSnapshot,
};
use super::config::DrainConfig;
use super::pumps::{
    drain_pump_best_effort, emit_drain_truncated, remaining_budget, topic_latest_id, PumpHandle,
    WaitpointSweepHandle,
};
use super::routing::{trigger_state_snapshots, ConnectorRuntime};
use super::CRON_TICK_TOPIC;
use super::STATE_SNAPSHOT_FILE;
use crate::package::CollectedManifestTrigger;

pub(super) struct GracefulShutdownCtx<'a> {
    pub(super) role: OrchestratorRole,
    pub(super) bind: SocketAddr,
    pub(super) listener_url: String,
    pub(super) config_path: &'a Path,
    pub(super) state_dir: &'a Path,
    pub(super) startup_started_at: &'a str,
    pub(super) event_log: &'a Arc<AnyEventLog>,
    pub(super) event_log_description: &'a harn_vm::event_log::EventLogDescription,
    pub(super) secret_chain_display: &'a str,
    pub(super) triggers: &'a [CollectedManifestTrigger],
    pub(super) connectors: &'a ConnectorRuntime,
    pub(super) shutdown_timeout: Duration,
    pub(super) drain_config: DrainConfig,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn graceful_shutdown(
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
