//! Topic pumps that drive the orchestrator's event-log → dispatcher pipeline.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::watch;
use tokio::task::JoinSet;

use harn_vm::event_log::{AnyEventLog, ConsumerId, EventLog};

use super::super::errors::OrchestratorError;
use super::audit::{
    admission_delay, append_lifecycle_event, append_pump_lifecycle_event, record_pump_metrics,
    wait_for_pump_drain_release,
};
use super::config::{DrainConfig, PumpConfig};
use super::{CRON_TICK_TOPIC, TEST_FAIL_PENDING_PUMP_ENV, TEST_INBOX_TASK_RELEASE_FILE_ENV};

const WAITPOINT_SERVICE_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub(super) struct PumpDrainGate {
    hold_tx: watch::Sender<bool>,
}

impl PumpDrainGate {
    pub(super) fn new() -> Self {
        let (hold_tx, _) = watch::channel(false);
        Self { hold_tx }
    }

    pub(super) fn pause(&self) {
        let _ = self.hold_tx.send(true);
    }

    pub(super) fn release(&self) {
        let _ = self.hold_tx.send(false);
    }

    fn subscribe(&self) -> watch::Receiver<bool> {
        self.hold_tx.subscribe()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PumpMode {
    Running,
    Draining(PumpDrainRequest),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PumpDrainRequest {
    pub(super) up_to: u64,
    pub(super) config: DrainConfig,
    pub(super) deadline: tokio::time::Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PumpDrainStopReason {
    Drained,
    MaxItems,
    Deadline,
    Error,
}

impl PumpDrainStopReason {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Drained => "drained",
            Self::MaxItems => "max_items",
            Self::Deadline => "deadline",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct PumpStats {
    pub(super) last_seen: u64,
    pub(super) processed: u64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PumpDrainProgress {
    pub(super) request: PumpDrainRequest,
    pub(super) start_seen: u64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PumpDrainReport {
    pub(super) stats: PumpStats,
    pub(super) drain_items: u64,
    pub(super) remaining_queued: u64,
    pub(super) outstanding_tasks: usize,
    pub(super) stop_reason: PumpDrainStopReason,
}

impl PumpDrainReport {
    pub(super) fn truncated(self) -> bool {
        self.remaining_queued > 0 || self.outstanding_tasks > 0
    }
}

pub(super) struct PumpHandle {
    mode_tx: watch::Sender<PumpMode>,
    join: tokio::task::JoinHandle<Result<PumpDrainReport, OrchestratorError>>,
}

impl PumpHandle {
    pub(super) async fn drain(
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

pub(super) struct WaitpointSweepHandle {
    stop_tx: watch::Sender<bool>,
    join: tokio::task::JoinHandle<Result<(), OrchestratorError>>,
}

impl WaitpointSweepHandle {
    pub(super) async fn shutdown(self) -> Result<(), OrchestratorError> {
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

pub(super) fn spawn_pending_pump(
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

pub(super) fn spawn_cron_pump(
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

pub(super) fn spawn_inbox_pump(
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

pub(super) fn spawn_waitpoint_resume_pump(
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

pub(super) fn spawn_waitpoint_cancel_pump(
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

pub(super) fn spawn_waitpoint_sweeper(dispatcher: harn_vm::Dispatcher) -> WaitpointSweepHandle {
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

// ── Drain helpers ─────────────────────────────────────────────────────────────

pub(super) async fn drain_pump_best_effort(
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
            eprintln!("[harn] warning: pump drain timed out for {topic_name} after {budget:?}");
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

pub(super) fn remaining_budget(deadline: tokio::time::Instant) -> Duration {
    deadline.saturating_duration_since(tokio::time::Instant::now())
}

pub(super) async fn emit_drain_truncated(
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

pub(super) async fn topic_latest_id(
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

pub(super) fn pump_consumer_id(
    topic: &harn_vm::event_log::Topic,
) -> Result<ConsumerId, OrchestratorError> {
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
