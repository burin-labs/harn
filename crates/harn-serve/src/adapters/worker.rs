//! Worker / job execution surface for `.harn` programs.
//!
//! `harn-serve` is HTTP-first: every other adapter answers a request over
//! a transport. Long-running, scheduled, and operator-batch programs need
//! a different shape — read a JSON request, do work, emit a JSON report —
//! while still inheriting the trigger dispatcher's retry, DLQ, budget,
//! cancellation, and audit behavior.
//!
//! Crucially this is **not** a second execution engine. A `@job` function
//! is lowered into a `harn_vm` [`TriggerBindingSpec`] whose handler is the
//! function's own closure, registered in the trigger registry, and
//! dispatched through the trigger [`Dispatcher`] — the same machinery that
//! already powers webhook / cron / queue triggers. Retry,
//! dead-letter-queue, per-dispatch budget, cancellation, and the
//! action-graph audit trail therefore come *for free* from the
//! dispatcher; the dispatcher needs zero changes to host jobs.
//!
//! ```text
//!   request.json ──▶ TriggerEvent (webhook payload `raw` = request)
//!                         │
//!   @job fn closure ──▶ TriggerBindingSpec{ handler: Local{closure} }
//!                         │  dynamic_register + resolve_live_trigger_binding
//!                         ▼
//!                   Dispatcher::dispatch(&binding, event)
//!                         │  retry / DLQ / budget / cancel (unchanged)
//!                         ▼
//!                   DispatchOutcome.result ──▶ report.json
//! ```
//!
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use futures::StreamExt;
use harn_vm::event_log::{install_default_for_base_dir, AnyEventLog, EventLog, Topic};
use harn_vm::triggers::event::{GenericWebhookPayload, KnownProviderPayload};
use harn_vm::{
    dynamic_register, resolve_live_trigger_binding, DispatchOutcome, DispatchStatus, Dispatcher,
    MetricsRegistry, ProviderId, ProviderPayload, RateLimitConfig, RateLimiterFactory, RetryPolicy,
    SignatureStatus, TriggerBindingSource, TriggerBindingSpec, TriggerEvent, TriggerHandlerSpec,
    TriggerRetryConfig, Vm, WorkerQueue, WorkerQueuePriority, WorkerQueueResponseRecord,
};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::limits::BudgetSpec;
use crate::{
    DispatchError, ExportCatalog, ExportedFunction, JobSpec, RetryBackoff, RetrySpec, ScheduleSpec,
};

/// Provider id stamped on synthetic job events. Reuses the generic
/// `webhook` payload variant so the request JSON rides in
/// `provider_payload.raw`, the idiomatic place `.harn` handlers read a
/// request body from (matching every other trigger handler).
const JOB_PROVIDER: &str = "webhook";
const CRON_PROVIDER: &str = "cron";
const CRON_KIND: &str = "cron";

const DEFAULT_CLAIM_TTL: StdDuration = StdDuration::from_mins(5);
const DEFAULT_SHUTDOWN_DRAIN: StdDuration = StdDuration::from_secs(30);

#[derive(Clone, Debug)]
pub struct WorkerServeOptions {
    pub consumer_id: Option<String>,
    pub claim_ttl: StdDuration,
    pub drain_timeout: StdDuration,
}

impl Default for WorkerServeOptions {
    fn default() -> Self {
        Self {
            consumer_id: None,
            claim_ttl: DEFAULT_CLAIM_TTL,
            drain_timeout: DEFAULT_SHUTDOWN_DRAIN,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerJobRegistration {
    pub job: String,
    pub function: String,
    pub binding_id: String,
    pub binding_key: String,
    pub binding_version: u32,
    pub schedule: Option<ScheduleSpec>,
    pub queue: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerShutdownReport {
    pub jobs: usize,
    pub queues: usize,
    pub drained: bool,
    pub in_flight: u64,
    pub retry_queue_depth: u64,
    pub dlq_depth: u64,
}

pub struct WorkerServer {
    event_log: Arc<AnyEventLog>,
    dispatcher: Dispatcher,
    cron_connector: Option<harn_vm::CronConnector>,
    shutdown_tx: broadcast::Sender<()>,
    tasks: Vec<JoinHandle<Result<(), DispatchError>>>,
    jobs: Vec<WorkerJobRegistration>,
    queues: BTreeSet<String>,
    drain_timeout: StdDuration,
}

impl WorkerServer {
    pub fn event_log(&self) -> Arc<AnyEventLog> {
        self.event_log.clone()
    }

    pub fn jobs(&self) -> &[WorkerJobRegistration] {
        &self.jobs
    }

    pub async fn shutdown(mut self) -> Result<WorkerShutdownReport, DispatchError> {
        let _ = self.shutdown_tx.send(());
        if let Some(connector) = self.cron_connector.take() {
            harn_vm::Connector::shutdown(&connector, self.drain_timeout)
                .await
                .map_err(|error| DispatchError::Execution(error.to_string()))?;
        }
        self.dispatcher.shutdown();

        for task in self.tasks {
            match task.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(error),
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    return Err(DispatchError::Execution(format!(
                        "worker task join failed: {error}"
                    )));
                }
            }
        }

        let drain = self
            .dispatcher
            .drain(self.drain_timeout)
            .await
            .map_err(|error| DispatchError::Execution(error.to_string()))?;
        Ok(WorkerShutdownReport {
            jobs: self.jobs.len(),
            queues: self.queues.len(),
            drained: drain.drained,
            in_flight: drain.in_flight,
            retry_queue_depth: drain.retry_queue_depth,
            dlq_depth: drain.dlq_depth,
        })
    }
}

/// Outcome of running one job dispatch. Thin wrapper over the trigger
/// [`DispatchOutcome`] so the CLI / factory worker can render the report
/// and pick an exit code without depending on `harn_vm` internals.
#[derive(Clone, Debug)]
pub struct JobRunOutcome {
    /// Job name (the `@job("name")` argument or the function name).
    pub job: String,
    /// Terminal dispatch status — `succeeded`, `dlq`, `failed`, …
    pub status: DispatchStatus,
    /// Number of attempts the dispatcher made (≥ 1 on success, up to the
    /// retry ceiling before a DLQ).
    pub attempt_count: u32,
    /// The value the `@job` function returned, JSON-encoded. `None` when
    /// the job failed before producing a result.
    pub result: Option<serde_json::Value>,
    /// Terminal error message when the job did not succeed.
    pub error: Option<String>,
}

impl JobRunOutcome {
    /// `true` when the dispatcher reported a successful terminal outcome.
    pub fn succeeded(&self) -> bool {
        matches!(self.status, DispatchStatus::Succeeded)
    }

    /// The report JSON to emit. Successful jobs render their returned
    /// value; failed jobs render a `{status, error}` envelope so the
    /// consumer always gets a JSON object on stdout.
    pub fn report_json(&self) -> serde_json::Value {
        match (&self.result, self.succeeded()) {
            (Some(value), true) => value.clone(),
            _ => serde_json::json!({
                "status": self.status.as_str(),
                "error": self.error.clone().unwrap_or_default(),
                "attempt_count": self.attempt_count,
            }),
        }
    }
}

struct PreparedJobRuntime {
    event_log: Arc<AnyEventLog>,
    vm: Vm,
    jobs: Vec<PreparedJob>,
}

struct PreparedJob {
    export: WorkerJobRegistration,
    budget: Option<BudgetSpec>,
}

/// Start a worker daemon for all `@job` exports in `script_path`.
///
/// The returned server owns local tasks for cron pumping, dispatcher
/// inbox dispatch, and worker-queue consumption. Call
/// [`WorkerServer::shutdown`] from the same `LocalSet` to stop them
/// gracefully.
pub async fn start_worker_server(
    script_path: &Path,
    options: WorkerServeOptions,
) -> Result<WorkerServer, DispatchError> {
    let prepared = prepare_job_runtime(script_path, |_vm| {}, None).await?;
    if prepared.jobs.is_empty() {
        return Err(DispatchError::Validation(format!(
            "{} does not export any `@job` functions",
            script_path.display()
        )));
    }

    let budgets_by_binding: Arc<BTreeMap<String, Option<BudgetSpec>>> = Arc::new(
        prepared
            .jobs
            .iter()
            .map(|job| (job.export.binding_id.clone(), job.budget.clone()))
            .collect(),
    );
    let jobs: Vec<WorkerJobRegistration> =
        prepared.jobs.iter().map(|job| job.export.clone()).collect();
    let queues: BTreeSet<String> = jobs.iter().filter_map(|job| job.queue.clone()).collect();

    let dispatcher = Dispatcher::with_event_log(prepared.vm, prepared.event_log.clone());
    let (shutdown_tx, _) = broadcast::channel(16);
    let mut tasks = Vec::new();
    tasks.push(spawn_inbox_pump(
        prepared.event_log.clone(),
        dispatcher.clone(),
        budgets_by_binding.clone(),
        shutdown_tx.subscribe(),
    )?);

    let has_scheduled_jobs = jobs.iter().any(|job| job.schedule.is_some());
    if has_scheduled_jobs {
        tasks.push(spawn_cron_pump(
            prepared.event_log.clone(),
            dispatcher.clone(),
            shutdown_tx.subscribe(),
        )?);
    }

    let consumer_id = options.consumer_id.unwrap_or_else(default_consumer_id);
    for queue_name in &queues {
        tasks.push(spawn_queue_consumer(
            prepared.event_log.clone(),
            dispatcher.clone(),
            queue_name.clone(),
            consumer_id.clone(),
            options.claim_ttl,
            budgets_by_binding.clone(),
            shutdown_tx.subscribe(),
        )?);
    }

    let mut cron_connector = harn_vm::CronConnector::new();
    if has_scheduled_jobs {
        let metrics = Arc::new(MetricsRegistry::default());
        let inbox = Arc::new(
            harn_vm::InboxIndex::new(prepared.event_log.clone(), metrics.clone())
                .await
                .map_err(|error| DispatchError::Execution(error.to_string()))?,
        );
        harn_vm::Connector::init(
            &mut cron_connector,
            harn_vm::ConnectorCtx {
                event_log: prepared.event_log.clone(),
                secrets: Arc::new(harn_vm::secrets::ChainSecretProvider::new(
                    "harn-worker",
                    Vec::<Arc<dyn harn_vm::secrets::SecretProvider>>::new(),
                )),
                inbox,
                metrics,
                rate_limiter: Arc::new(RateLimiterFactory::new(RateLimitConfig::default())),
            },
        )
        .await
        .map_err(|error| DispatchError::Execution(error.to_string()))?;
        let cron_bindings = jobs
            .iter()
            .filter_map(cron_connector_binding)
            .collect::<Vec<_>>();
        harn_vm::Connector::activate(&cron_connector, &cron_bindings)
            .await
            .map_err(|error| DispatchError::Execution(error.to_string()))?;
    }

    Ok(WorkerServer {
        event_log: prepared.event_log,
        dispatcher,
        cron_connector: has_scheduled_jobs.then_some(cron_connector),
        shutdown_tx,
        tasks,
        jobs,
        queues,
        drain_timeout: options.drain_timeout,
    })
}

async fn prepare_job_runtime(
    script_path: &Path,
    configure: impl FnOnce(&mut Vm),
    retry_override: Option<&TriggerRetryConfig>,
) -> Result<PreparedJobRuntime, DispatchError> {
    harn_vm::reset_thread_local_state();
    harn_vm::clear_trigger_registry();
    harn_vm::clear_dispatcher_state();

    let script_path = std::fs::canonicalize(script_path).map_err(|error| {
        DispatchError::Io(format!(
            "failed to resolve job script {}: {error}",
            script_path.display()
        ))
    })?;
    let script_path = script_path.as_path();

    let catalog = ExportCatalog::from_path(script_path)?;
    crate::emit_export_diagnostics(catalog.diagnostics());
    validate_unique_job_names(&catalog)?;

    let base_dir = script_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let event_log = install_default_for_base_dir(&base_dir).map_err(|error| {
        DispatchError::Io(format!(
            "failed to initialize event log for {}: {error}",
            base_dir.display()
        ))
    })?;

    let mut vm = Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    harn_vm::register_store_builtins(&mut vm, &base_dir);
    harn_vm::register_metadata_builtins(&mut vm, &base_dir);
    vm.set_source_dir(&base_dir);
    vm.set_harness(harn_vm::Harness::real());
    configure(&mut vm);

    let exports = vm
        .load_module_exports(script_path)
        .await
        .map_err(|error| DispatchError::Execution(error.to_string()))?;

    let mut jobs = Vec::new();
    for function in catalog.functions.values() {
        let Some(job) = function.job.clone() else {
            continue;
        };
        let closure = exports.get(&function.name).cloned().ok_or_else(|| {
            DispatchError::MissingExport(format!(
                "function '{}' is not exported by {}",
                function.name,
                script_path.display()
            ))
        })?;
        let spec = job_binding_spec(&job, function, closure, retry_override);
        let binding_id = dynamic_register(spec)
            .await
            .map_err(|error| DispatchError::Execution(error.to_string()))?;
        let binding = resolve_live_trigger_binding(binding_id.as_str(), None)
            .map_err(|error| DispatchError::Execution(error.to_string()))?;
        jobs.push(PreparedJob {
            export: WorkerJobRegistration {
                job: job.name.clone(),
                function: function.name.clone(),
                binding_id: binding_id.as_str().to_string(),
                binding_key: binding.binding_key(),
                binding_version: binding.version,
                schedule: job.schedule.clone(),
                queue: job.queue.clone(),
            },
            budget: function.budget.clone(),
        });
    }

    Ok(PreparedJobRuntime {
        event_log,
        vm,
        jobs,
    })
}

fn validate_unique_job_names(catalog: &ExportCatalog) -> Result<(), DispatchError> {
    let mut seen = BTreeSet::new();
    for function in catalog.functions.values() {
        let Some(job) = function.job.as_ref() else {
            continue;
        };
        if !seen.insert(job.name.clone()) {
            return Err(DispatchError::Validation(format!(
                "multiple `@job(\"{}\")` exports found in {}; job names must be unique",
                job.name,
                catalog.script_path.display()
            )));
        }
    }
    Ok(())
}

fn cron_connector_binding(job: &WorkerJobRegistration) -> Option<harn_vm::TriggerBinding> {
    let schedule = job.schedule.as_ref()?;
    let mut binding = harn_vm::TriggerBinding::new(
        ProviderId::from(CRON_PROVIDER),
        harn_vm::TriggerKind::from(CRON_KIND),
        job.binding_id.clone(),
    );
    binding.config = serde_json::json!({
        "schedule": schedule.cron,
        "timezone": schedule.timezone.as_deref().unwrap_or("UTC"),
        "retention_days": harn_vm::DEFAULT_INBOX_RETENTION_DAYS,
    });
    Some(binding)
}

fn spawn_cron_pump(
    event_log: Arc<AnyEventLog>,
    dispatcher: Dispatcher,
    shutdown_rx: broadcast::Receiver<()>,
) -> Result<JoinHandle<Result<(), DispatchError>>, DispatchError> {
    let topic = Topic::new(harn_vm::connectors::cron::CRON_TICK_TOPIC)
        .map_err(|error| DispatchError::Execution(error.to_string()))?;
    Ok(tokio::task::spawn_local(run_cron_pump(
        event_log,
        dispatcher,
        topic,
        shutdown_rx,
    )))
}

async fn run_cron_pump(
    event_log: Arc<AnyEventLog>,
    dispatcher: Dispatcher,
    topic: Topic,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<(), DispatchError> {
    let start_from = event_log
        .latest(&topic)
        .await
        .map_err(|error| DispatchError::Execution(error.to_string()))?;
    let mut stream = event_log
        .clone()
        .subscribe(&topic, start_from)
        .await
        .map_err(|error| DispatchError::Execution(error.to_string()))?;

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => break,
            received = stream.next() => {
                let Some(received) = received else {
                    break;
                };
                let (_, logged) = received
                    .map_err(|error| DispatchError::Execution(error.to_string()))?;
                if logged.kind != "trigger_event" {
                    continue;
                }
                let event: TriggerEvent = serde_json::from_value(logged.payload)
                    .map_err(|error| DispatchError::Execution(format!("failed to decode cron trigger event: {error}")))?;
                let trigger_id = match &event.provider_payload {
                    ProviderPayload::Known(KnownProviderPayload::Cron(payload)) => {
                        payload.cron_id.clone()
                    }
                    _ => None,
                };
                dispatcher
                    .enqueue_targeted_with_headers(trigger_id, None, event, Some(&logged.headers))
                    .await
                    .map_err(|error| DispatchError::Execution(error.to_string()))?;
            }
        }
    }
    Ok(())
}

fn spawn_inbox_pump(
    event_log: Arc<AnyEventLog>,
    dispatcher: Dispatcher,
    budgets_by_binding: Arc<BTreeMap<String, Option<BudgetSpec>>>,
    shutdown_rx: broadcast::Receiver<()>,
) -> Result<JoinHandle<Result<(), DispatchError>>, DispatchError> {
    let topic = Topic::new(harn_vm::TRIGGER_INBOX_ENVELOPES_TOPIC)
        .map_err(|error| DispatchError::Execution(error.to_string()))?;
    Ok(tokio::task::spawn_local(run_inbox_pump(
        event_log,
        dispatcher,
        budgets_by_binding,
        topic,
        shutdown_rx,
    )))
}

async fn run_inbox_pump(
    event_log: Arc<AnyEventLog>,
    dispatcher: Dispatcher,
    budgets_by_binding: Arc<BTreeMap<String, Option<BudgetSpec>>>,
    topic: Topic,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<(), DispatchError> {
    let start_from = event_log
        .latest(&topic)
        .await
        .map_err(|error| DispatchError::Execution(error.to_string()))?;
    let mut stream = event_log
        .clone()
        .subscribe(&topic, start_from)
        .await
        .map_err(|error| DispatchError::Execution(error.to_string()))?;

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => break,
            received = stream.next() => {
                let Some(received) = received else {
                    break;
                };
                let (_, logged) = received
                    .map_err(|error| DispatchError::Execution(error.to_string()))?;
                if logged.kind != "event_ingested" {
                    continue;
                }
                let envelope: harn_vm::triggers::dispatcher::InboxEnvelope =
                    serde_json::from_value(logged.payload)
                        .map_err(|error| DispatchError::Execution(format!("failed to decode dispatcher inbox event: {error}")))?;
                let budget = budget_for_envelope(&envelope, &budgets_by_binding).cloned().flatten();
                let _budget_guard = budget.as_ref().and_then(BudgetSpec::install);
                dispatcher
                    .dispatch_inbox_envelope_with_parent_headers(envelope, &logged.headers)
                    .await
                    .map_err(|error| DispatchError::Execution(error.to_string()))?;
            }
        }
    }
    Ok(())
}

fn spawn_queue_consumer(
    event_log: Arc<AnyEventLog>,
    dispatcher: Dispatcher,
    queue_name: String,
    consumer_id: String,
    claim_ttl: StdDuration,
    budgets_by_binding: Arc<BTreeMap<String, Option<BudgetSpec>>>,
    shutdown_rx: broadcast::Receiver<()>,
) -> Result<JoinHandle<Result<(), DispatchError>>, DispatchError> {
    let topic = Topic::new(harn_vm::worker_job_topic_name(&queue_name))
        .map_err(|error| DispatchError::Execution(error.to_string()))?;
    Ok(tokio::task::spawn_local(run_queue_consumer(
        event_log,
        dispatcher,
        queue_name,
        consumer_id,
        claim_ttl,
        budgets_by_binding,
        topic,
        shutdown_rx,
    )))
}

async fn run_queue_consumer(
    event_log: Arc<AnyEventLog>,
    dispatcher: Dispatcher,
    queue_name: String,
    consumer_id: String,
    claim_ttl: StdDuration,
    budgets_by_binding: Arc<BTreeMap<String, Option<BudgetSpec>>>,
    topic: Topic,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<(), DispatchError> {
    let start_from = event_log
        .latest(&topic)
        .await
        .map_err(|error| DispatchError::Execution(error.to_string()))?;
    let mut stream = event_log
        .clone()
        .subscribe(&topic, start_from)
        .await
        .map_err(|error| DispatchError::Execution(error.to_string()))?;
    let queue = WorkerQueue::new(event_log);

    drain_queue(
        &queue,
        &dispatcher,
        &queue_name,
        &consumer_id,
        claim_ttl,
        &budgets_by_binding,
    )
    .await?;

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => break,
            received = stream.next() => {
                let Some(received) = received else {
                    break;
                };
                let (_, logged) = received
                    .map_err(|error| DispatchError::Execution(error.to_string()))?;
                if logged.kind == "trigger_dispatch" {
                    drain_queue(
                        &queue,
                        &dispatcher,
                        &queue_name,
                        &consumer_id,
                        claim_ttl,
                        &budgets_by_binding,
                    )
                    .await?;
                }
            }
        }
    }
    Ok(())
}

async fn drain_queue(
    queue: &WorkerQueue,
    dispatcher: &Dispatcher,
    queue_name: &str,
    consumer_id: &str,
    claim_ttl: StdDuration,
    budgets_by_binding: &BTreeMap<String, Option<BudgetSpec>>,
) -> Result<(), DispatchError> {
    loop {
        let Some(claimed) = queue
            .claim_next(queue_name, consumer_id, claim_ttl)
            .await
            .map_err(|error| {
                DispatchError::Execution(format!("failed to claim worker job: {error}"))
            })?
        else {
            break;
        };

        let heartbeat = start_claim_heartbeat(queue.clone(), claimed.handle.clone(), claim_ttl);
        let response = match resolve_live_trigger_binding(&claimed.job.trigger_id, None) {
            Ok(binding) if matches!(binding.handler, TriggerHandlerSpec::Worker { .. }) => {
                WorkerQueueResponseRecord {
                    queue: queue_name.to_string(),
                    job_event_id: claimed.handle.job_event_id,
                    consumer_id: consumer_id.to_string(),
                    handled_at_ms: now_ms(),
                    outcome: None,
                    error: Some(format!(
                        "worker queue '{}' resolved trigger '{}' to another worker:// handler; queue consumers require a non-worker binding",
                        queue_name, claimed.job.trigger_id
                    )),
                }
            }
            Ok(binding) => {
                let budget = budgets_by_binding.get(&claimed.job.trigger_id).cloned().flatten();
                let _budget_guard = budget.as_ref().and_then(BudgetSpec::install);
                match dispatcher.dispatch(&binding, claimed.job.event.clone()).await {
                    Ok(outcome) => WorkerQueueResponseRecord {
                        queue: queue_name.to_string(),
                        job_event_id: claimed.handle.job_event_id,
                        consumer_id: consumer_id.to_string(),
                        handled_at_ms: now_ms(),
                        outcome: Some(outcome),
                        error: None,
                    },
                    Err(error) => WorkerQueueResponseRecord {
                        queue: queue_name.to_string(),
                        job_event_id: claimed.handle.job_event_id,
                        consumer_id: consumer_id.to_string(),
                        handled_at_ms: now_ms(),
                        outcome: None,
                        error: Some(error.to_string()),
                    },
                }
            }
            Err(error) => WorkerQueueResponseRecord {
                queue: queue_name.to_string(),
                job_event_id: claimed.handle.job_event_id,
                consumer_id: consumer_id.to_string(),
                handled_at_ms: now_ms(),
                outcome: None,
                error: Some(format!(
                    "failed to resolve worker binding '{}': {error}",
                    claimed.job.trigger_id
                )),
            },
        };

        stop_claim_heartbeat(heartbeat).await;
        queue
            .append_response(queue_name, &response)
            .await
            .map_err(|error| {
                DispatchError::Execution(format!("failed to append worker response: {error}"))
            })?;
        let should_ack = response.error.is_none()
            && response.outcome.as_ref().is_some_and(|outcome| {
                matches!(
                    outcome.status,
                    DispatchStatus::Succeeded | DispatchStatus::Skipped | DispatchStatus::Dlq
                )
            });
        if should_ack {
            queue.ack_claim(&claimed.handle).await.map_err(|error| {
                DispatchError::Execution(format!("failed to ack worker claim: {error}"))
            })?;
        }
    }
    Ok(())
}

fn budget_for_envelope<'a>(
    envelope: &harn_vm::triggers::dispatcher::InboxEnvelope,
    budgets_by_binding: &'a BTreeMap<String, Option<BudgetSpec>>,
) -> Option<&'a Option<BudgetSpec>> {
    if let Some(trigger_id) = envelope.trigger_id.as_ref() {
        return budgets_by_binding.get(trigger_id);
    }
    match &envelope.event.provider_payload {
        ProviderPayload::Known(KnownProviderPayload::Cron(payload)) => payload
            .cron_id
            .as_ref()
            .and_then(|trigger_id| budgets_by_binding.get(trigger_id)),
        _ => None,
    }
}

fn start_claim_heartbeat(
    queue: WorkerQueue,
    handle: harn_vm::WorkerQueueClaimHandle,
    ttl: StdDuration,
) -> (watch::Sender<bool>, JoinHandle<()>) {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let interval = heartbeat_interval(ttl);
    let join = tokio::task::spawn_local(async move {
        loop {
            tokio::select! {
                changed = stop_rx.changed() => {
                    if changed.is_err() || *stop_rx.borrow() {
                        break;
                    }
                }
                _ = tokio::time::sleep(interval) => {
                    if queue.renew_claim(&handle, ttl).await.unwrap_or(false) {
                        continue;
                    }
                    break;
                }
            }
        }
    });
    (stop_tx, join)
}

async fn stop_claim_heartbeat(heartbeat: (watch::Sender<bool>, JoinHandle<()>)) {
    let (stop_tx, join) = heartbeat;
    let _ = stop_tx.send(true);
    let _ = join.await;
}

fn heartbeat_interval(ttl: StdDuration) -> StdDuration {
    let millis = ttl.as_millis() as u64;
    StdDuration::from_millis((millis / 2).clamp(250, 30_000))
}

fn default_consumer_id() -> String {
    format!(
        "harn-worker-pid{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    )
}

fn now_ms() -> i64 {
    harn_vm::clock_mock::now_ms()
}

/// Driver-level knobs for a one-shot `@job` run.
///
/// The default is **exactly** the production behaviour: no override, so the
/// dispatcher uses the `@job`'s declared `@retry`/`retry:` policy (or its
/// own default). Overrides are opt-in — they let a one-shot or failure-path
/// test runner cap or disable retry without editing the `@job` source, so an
/// erroring job fails fast instead of sleeping through a multi-hour backoff.
#[derive(Clone, Debug, Default)]
pub struct JobRunOptions {
    /// When set, this retry config replaces the `@job`'s declared policy for
    /// this run only. `None` (the default) preserves production behaviour.
    pub retry_override: Option<TriggerRetryConfig>,
}

impl JobRunOptions {
    /// Override the retry policy for this run, replacing the `@job`'s
    /// declared policy. The dispatcher's binding is otherwise unchanged.
    pub fn with_retry(mut self, retry: TriggerRetryConfig) -> Self {
        self.retry_override = Some(retry);
        self
    }

    /// Run the `@job` at most once: a single attempt with no retry and no
    /// backoff sleep. The natural choice for one-shot CLI / failure-path
    /// test drivers that must fail fast rather than inherit a long backoff.
    ///
    /// `max_attempts: 1` makes `TriggerRetryConfig::next_retry_delay` return
    /// `None` after the first attempt, so the dispatcher never sleeps. The
    /// `Linear { delay_ms: 0 }` policy is belt-and-suspenders: even the
    /// first-attempt delay is zero.
    pub fn fail_fast() -> Self {
        Self {
            retry_override: Some(TriggerRetryConfig::new(
                1,
                RetryPolicy::Linear { delay_ms: 0 },
            )),
        }
    }
}

/// Run one `@job` function against a single JSON request and return its
/// outcome. This is the one-shot driver behind `harn run --as-job`.
///
/// Uses the `@job`'s declared retry policy unchanged. To cap or disable
/// retry for a one-shot/failure-path run, use [`run_job_once_with_options`]
/// with [`JobRunOptions::fail_fast`].
///
/// Mirrors [`crate::core::DispatchCore::invoke_function`] for the base-VM
/// build (stdlib + store/metadata builtins + real harness), then hands
/// the rest of the lifecycle to the trigger dispatcher.
pub async fn run_job_once(
    script_path: &Path,
    job_name: &str,
    request: serde_json::Value,
) -> Result<JobRunOutcome, DispatchError> {
    run_job_once_with(script_path, job_name, request, |_vm| {}).await
}

/// Like [`run_job_once`], but lets the embedder inject extra VM state via a
/// `configure` closure that runs on the fully-built job VM.
///
/// The closure receives `&mut Vm` *after* the standard registration
/// (`register_vm_stdlib` + `register_store_builtins` +
/// `register_metadata_builtins` + source-dir/harness wiring) and *before*
/// the job module is loaded and the entrypoint executes. This lets an
/// embedder register host-defined builtins (e.g. a `sandbox_exec` that
/// bridges to a cloud-sandbox adapter) that coexist with the standard
/// ones, so the `@job` closure can call them.
///
/// Ordering guarantees:
/// - Standard stdlib + store/metadata builtins are registered first, so
///   embedder builtins may *extend* the surface the job sees.
/// - Embedder builtins are registered last, so a name collision *overrides*
///   the standard builtin (`register_builtin` replaces by name).
/// - The closure runs before `load_module_exports`, so the job module's
///   captured globals resolve against the embedder-augmented VM.
pub async fn run_job_once_with(
    script_path: &Path,
    job_name: &str,
    request: serde_json::Value,
    configure: impl FnOnce(&mut Vm),
) -> Result<JobRunOutcome, DispatchError> {
    run_job_once_with_options(
        script_path,
        job_name,
        request,
        JobRunOptions::default(),
        configure,
    )
    .await
}

/// Like [`run_job_once_with`], but also accepts [`JobRunOptions`] so a
/// driver can override the `@job`'s retry policy for this run (e.g.
/// [`JobRunOptions::fail_fast`] to run a single attempt with no backoff).
///
/// With [`JobRunOptions::default`] the behaviour is identical to
/// [`run_job_once_with`]: the `@job`'s declared policy is used unchanged.
pub async fn run_job_once_with_options(
    script_path: &Path,
    job_name: &str,
    request: serde_json::Value,
    options: JobRunOptions,
    configure: impl FnOnce(&mut Vm),
) -> Result<JobRunOutcome, DispatchError> {
    let prepared =
        prepare_job_runtime(script_path, configure, options.retry_override.as_ref()).await?;
    let job = prepared
        .jobs
        .iter()
        .find(|job| job.export.job == job_name)
        .ok_or_else(|| {
            DispatchError::MissingExport(format!(
                "no `@job(\"{job_name}\")` exported by {}",
                script_path.display()
            ))
        })?;
    let binding = resolve_live_trigger_binding(&job.export.binding_id, None)
        .map_err(|error| DispatchError::Execution(error.to_string()))?;
    let _budget_guard = job.budget.as_ref().and_then(BudgetSpec::install);

    let event = job_event(&job.export.job, request)?;
    let dispatcher = Dispatcher::with_event_log(prepared.vm, prepared.event_log);
    let outcome = dispatcher
        .dispatch(&binding, event)
        .await
        .map_err(|error| DispatchError::Execution(error.to_string()))?;

    Ok(job_run_outcome(&job.export.job, outcome))
}

/// Lower a parsed [`JobSpec`] (+ its `@budget`/`@scopes`) into the trigger
/// binding the dispatcher consumes. The handler is the function's own
/// closure, so dispatch executes the user's `.harn` code directly.
fn job_binding_spec(
    job: &JobSpec,
    function: &ExportedFunction,
    closure: Arc<harn_vm::VmClosure>,
    retry_override: Option<&TriggerRetryConfig>,
) -> TriggerBindingSpec {
    // A driver-level override (e.g. a one-shot/test runner that wants to
    // fail fast) takes precedence over the `@job`'s declared policy. When
    // no override is given, behaviour is exactly as before: the `@job`'s
    // declared `@retry`/`retry:` policy, or the dispatcher default.
    let retry = match retry_override {
        Some(config) => config.clone(),
        None => job.retry.as_ref().map(retry_config).unwrap_or_default(),
    };

    // Scheduled jobs register with the cron provider so cron connector
    // ticks target the same binding the one-shot and queue paths use.
    let (provider, kind) = if job.schedule.is_some() {
        (CRON_PROVIDER, CRON_KIND)
    } else {
        (JOB_PROVIDER, "job")
    };

    TriggerBindingSpec {
        id: format!("job:{}", job.name),
        source: TriggerBindingSource::Dynamic,
        kind: kind.to_string(),
        provider: ProviderId::from(provider),
        autonomy_tier: harn_vm::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::Local {
            raw: function.name.clone(),
            closure,
        },
        dispatch_priority: WorkerQueuePriority::Normal,
        when: None,
        when_budget: None,
        retry,
        match_events: Vec::new(),
        dedupe_key: None,
        dedupe_retention_days: harn_vm::DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd: None,
        hourly_cost_usd: None,
        max_autonomous_decisions_per_hour: None,
        max_autonomous_decisions_per_day: None,
        on_budget_exhausted: harn_vm::TriggerBudgetExhaustionStrategy::False,
        max_concurrent: None,
        flow_control: harn_vm::TriggerFlowControlConfig::default(),
        aggregation: None,
        manifest_path: None,
        package_name: None,
        definition_fingerprint: format!("job:{}:v1", job.name),
    }
}

/// Map a parsed [`RetrySpec`] onto the dispatcher's retry config. Linear /
/// exponential pick conservative defaults the author can later tune via
/// the full trigger DSL; the keyword is what `@retry(backoff:)` exposes.
fn retry_config(spec: &RetrySpec) -> TriggerRetryConfig {
    let policy = match spec.backoff {
        RetryBackoff::Svix => RetryPolicy::Svix,
        RetryBackoff::Linear => RetryPolicy::Linear { delay_ms: 1_000 },
        RetryBackoff::Exponential => RetryPolicy::Exponential {
            base_ms: 1_000,
            cap_ms: 60_000,
        },
    };
    // `max_attempts == 0` means "defer to the dispatcher default", which
    // `TriggerRetryConfig::max_attempts()` already honours.
    TriggerRetryConfig::new(spec.max_attempts, policy)
}

/// Wrap a request JSON object as a synthetic [`TriggerEvent`]. The request
/// rides in the generic-webhook payload's `raw` field, so the `@job`
/// handler reads it as `event.provider_payload.raw` — the same place
/// every other webhook-shaped trigger handler reads its body.
fn job_event(job_name: &str, request: serde_json::Value) -> Result<TriggerEvent, DispatchError> {
    Ok(TriggerEvent::new(
        ProviderId::from(JOB_PROVIDER),
        "job",
        None,
        format!("job:{job_name}:{}", uuid::Uuid::new_v4()),
        None,
        std::collections::BTreeMap::new(),
        ProviderPayload::Known(KnownProviderPayload::Webhook(GenericWebhookPayload {
            source: Some(format!("job:{job_name}")),
            content_type: Some("application/json".to_string()),
            raw: request,
        })),
        SignatureStatus::Verified,
    ))
}

fn job_run_outcome(job_name: &str, outcome: DispatchOutcome) -> JobRunOutcome {
    JobRunOutcome {
        job: job_name.to_string(),
        status: outcome.status,
        attempt_count: outcome.attempt_count,
        result: outcome.result,
        error: outcome.error,
    }
}

/// Read a JSON request from `request_path`, run the named `@job` in
/// `script_path`, and (optionally) write the report JSON to
/// `result_out`. Always returns the rendered report string for the CLI to
/// print, plus the outcome for exit-code selection.
///
/// This supports file-oriented worker entrypoints that receive a request
/// JSON document and emit one report JSON document.
pub async fn run_job_from_files(
    script_path: &Path,
    job_name: &str,
    request_path: &Path,
    result_out: Option<&Path>,
    pretty: bool,
) -> Result<(JobRunOutcome, String), DispatchError> {
    let raw = std::fs::read_to_string(request_path).map_err(|error| {
        DispatchError::Io(format!(
            "failed to read request {}: {error}",
            request_path.display()
        ))
    })?;
    let request: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
        DispatchError::Validation(format!(
            "request {} is not valid JSON: {error}",
            request_path.display()
        ))
    })?;

    let outcome = run_job_once(script_path, job_name, request).await?;
    let report = outcome.report_json();
    let rendered = if pretty {
        serde_json::to_string_pretty(&report)
    } else {
        serde_json::to_string(&report)
    }
    .map_err(|error| DispatchError::Execution(format!("failed to render report JSON: {error}")))?;

    if let Some(out) = result_out {
        std::fs::write(out, &rendered).map_err(|error| {
            DispatchError::Io(format!("failed to write report {}: {error}", out.display()))
        })?;
    }

    Ok((outcome, rendered))
}

/// Convenience for callers that only have a script path string.
pub fn script_path_buf(path: &str) -> PathBuf {
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct ScopedEnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl ScopedEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    async fn write_script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("worker.harn");
        tokio::fs::write(&path, body).await.expect("write script");
        path
    }

    async fn wait_for_log_event(
        event_log: Arc<AnyEventLog>,
        topic_name: &str,
        matches: impl Fn(&harn_vm::event_log::LogEvent) -> bool,
    ) -> harn_vm::event_log::LogEvent {
        let topic = Topic::new(topic_name).expect("test topic is valid");
        let latest = event_log.latest(&topic).await.expect("latest event id");
        let mut stream = event_log
            .clone()
            .subscribe(&topic, latest)
            .await
            .expect("subscribe to topic");

        for (_, event) in event_log
            .read_range(&topic, None, usize::MAX)
            .await
            .expect("read topic")
        {
            if matches(&event) {
                return event;
            }
        }

        tokio::time::timeout(StdDuration::from_secs(5), async {
            loop {
                let Some(received) = stream.next().await else {
                    panic!("event stream ended before matching event");
                };
                let (_, event) = received.expect("read event");
                if matches(&event) {
                    return event;
                }
            }
        })
        .await
        .expect("matching event")
    }

    async fn wait_for_attempt(
        event_log: Arc<AnyEventLog>,
        trigger_id: &str,
    ) -> harn_vm::event_log::LogEvent {
        wait_for_log_event(event_log, harn_vm::TRIGGER_ATTEMPTS_TOPIC, |event| {
            event.kind == "attempt_recorded"
                && event
                    .payload
                    .get("trigger_id")
                    .and_then(|value| value.as_str())
                    == Some(trigger_id)
        })
        .await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_shot_job_echoes_request_and_succeeds() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan")
pub fn scan(event: TriggerEvent) -> dict {
  let req = event.provider_payload.raw
  return {status: "ok", echo: req}
}
"#,
                )
                .await;

                let request = serde_json::json!({"repo": "burin-labs/harn", "n": 7});
                let outcome = run_job_once(&script, "scan", request.clone())
                    .await
                    .expect("run job");

                assert_eq!(outcome.status, DispatchStatus::Succeeded);
                assert_eq!(outcome.attempt_count, 1);
                let result = outcome.result.expect("result");
                assert_eq!(result["status"], serde_json::json!("ok"));
                assert_eq!(result["echo"], request);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_shot_resolves_public_job_name_not_function_name() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan")
pub fn run_scan(event: TriggerEvent) -> dict {
  return {status: "ok", echo: event.provider_payload.raw}
}
"#,
                )
                .await;

                let outcome = run_job_once(&script, "scan", serde_json::json!({"id": 7}))
                    .await
                    .expect("run job by public name");

                assert_eq!(outcome.job, "scan");
                assert_eq!(outcome.status, DispatchStatus::Succeeded);
                assert_eq!(
                    outcome.result.expect("result")["echo"],
                    serde_json::json!({"id": 7})
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn configure_hook_registers_callable_host_builtin() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan")
pub fn scan(event: TriggerEvent) -> dict {
  let req = event.provider_payload.raw
  return {status: "ok", host: host_echo(req.repo)}
}
"#,
                )
                .await;

                let request = serde_json::json!({"repo": "burin-labs/harn"});
                let outcome = run_job_once_with(&script, "scan", request, |vm| {
                    // An embedder-defined builtin, injected via the configure
                    // hook on the fully-built job VM. The `@job` closure calls
                    // it by bare name, exactly like the stdlib builtins.
                    vm.register_builtin("host_echo", |args, _out| {
                        let x = args.first().map(|a| a.display()).unwrap_or_default();
                        Ok(harn_vm::VmValue::String(arcstr::ArcStr::from(
                            format!("host:{x}").as_str(),
                        )))
                    });
                })
                .await
                .expect("run job");

                assert_eq!(outcome.status, DispatchStatus::Succeeded);
                let result = outcome.result.expect("result");
                assert_eq!(result["status"], serde_json::json!("ok"));
                assert_eq!(result["host"], serde_json::json!("host:burin-labs/harn"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handler_error_retries_then_dlqs() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan")
@retry(max: 2, backoff: "linear")
pub fn scan(event: TriggerEvent) -> dict {
  throw "boom"
}
"#,
                )
                .await;

                let outcome = run_job_once(&script, "scan", serde_json::json!({}))
                    .await
                    .expect("run job returns terminal outcome");

                assert_eq!(outcome.status, DispatchStatus::Dlq);
                assert_eq!(outcome.attempt_count, 2);
                assert!(!outcome.succeeded());
                // The rendered report is a JSON object even on failure.
                let report = outcome.report_json();
                assert_eq!(report["status"], serde_json::json!("dlq"));
                assert!(report["error"].as_str().is_some_and(|e| e.contains("boom")));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compact_job_retry_dict_still_maps_to_dispatcher_retry() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan", retry: { max: 2, backoff: "linear" })
pub fn scan(event: TriggerEvent) -> dict {
  throw "boom"
}
"#,
                )
                .await;

                let outcome = run_job_once(&script, "scan", serde_json::json!({}))
                    .await
                    .expect("run job returns terminal outcome");

                assert_eq!(outcome.status, DispatchStatus::Dlq);
                assert_eq!(outcome.attempt_count, 2);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fail_fast_override_runs_a_single_attempt_for_an_erroring_job() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                // The `@job` declares the production default (svix, max 7),
                // whose backoff would sleep minutes-to-hours between
                // attempts. The driver override must cap it to one attempt.
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan")
@retry(max: 7, backoff: "svix")
pub fn scan(event: TriggerEvent) -> dict {
  throw "boom"
}
"#,
                )
                .await;

                let started = std::time::Instant::now();
                let outcome = run_job_once_with_options(
                    &script,
                    "scan",
                    serde_json::json!({}),
                    JobRunOptions::fail_fast(),
                    |_vm| {},
                )
                .await
                .expect("run job returns terminal outcome");
                let elapsed = started.elapsed();

                // One attempt, no retry, no backoff sleep: terminal failure
                // arrives effectively immediately despite the `@job`'s
                // multi-hour svix policy.
                assert_eq!(outcome.attempt_count, 1);
                assert_eq!(outcome.status, DispatchStatus::Dlq);
                assert!(!outcome.succeeded());
                assert!(
                    elapsed < StdDuration::from_secs(5),
                    "fail-fast run should not sleep through retry backoff (took {elapsed:?})"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn retry_override_caps_attempts_below_the_job_policy() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                // Declared policy is 5 attempts; the driver caps it to 3 with
                // an immediate (zero-delay) backoff so the test is fast.
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan")
@retry(max: 5, backoff: "linear")
pub fn scan(event: TriggerEvent) -> dict {
  throw "boom"
}
"#,
                )
                .await;

                let outcome = run_job_once_with_options(
                    &script,
                    "scan",
                    serde_json::json!({}),
                    JobRunOptions::default().with_retry(TriggerRetryConfig::new(
                        3,
                        RetryPolicy::Linear { delay_ms: 0 },
                    )),
                    |_vm| {},
                )
                .await
                .expect("run job returns terminal outcome");

                assert_eq!(outcome.attempt_count, 3);
                assert_eq!(outcome.status, DispatchStatus::Dlq);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn default_options_preserve_the_job_declared_retry_policy() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                // Linear backoff: attempt 1 is immediate, attempt 2 sleeps
                // 1s, so the default (no override) path stays fast enough to
                // test while still proving the `@job`'s `max: 2` is honoured.
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan")
@retry(max: 2, backoff: "linear")
pub fn scan(event: TriggerEvent) -> dict {
  throw "boom"
}
"#,
                )
                .await;

                // `JobRunOptions::default()` carries no override, so the
                // dispatcher must use the `@job`'s declared `max: 2`.
                let outcome = run_job_once_with_options(
                    &script,
                    "scan",
                    serde_json::json!({}),
                    JobRunOptions::default(),
                    |_vm| {},
                )
                .await
                .expect("run job returns terminal outcome");

                assert_eq!(outcome.attempt_count, 2);
                assert_eq!(outcome.status, DispatchStatus::Dlq);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worker_server_activates_scheduled_jobs() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let _env_lock = ENV_LOCK.lock().await;
                let _single_tick = ScopedEnvVar::set("HARN_TEST_CRON_SINGLE_TICK_AT", "1700000000");
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("tick")
@schedule("* * * * *", "UTC")
pub fn run_tick(event: TriggerEvent) -> dict {
  return {status: "ok"}
}
"#,
                )
                .await;

                let server = start_worker_server(
                    &script,
                    WorkerServeOptions {
                        drain_timeout: StdDuration::from_secs(5),
                        ..WorkerServeOptions::default()
                    },
                )
                .await
                .expect("start worker server");
                assert_eq!(server.jobs().len(), 1);
                assert_eq!(server.jobs()[0].job, "tick");

                let event_log = server.event_log();
                let attempt = wait_for_attempt(event_log, "job:tick").await;
                assert_eq!(attempt.payload["outcome"], serde_json::json!("success"));

                let report = server.shutdown().await.expect("shutdown worker");
                assert!(report.drained);
                assert_eq!(report.jobs, 1);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn worker_server_consumes_worker_queue_jobs() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan")
@queue("scan-jobs")
pub fn scan(event: TriggerEvent) -> dict {
  return {status: "ok", echo: event.provider_payload.raw}
}
"#,
                )
                .await;

                let server = start_worker_server(
                    &script,
                    WorkerServeOptions {
                        consumer_id: Some("test-worker".to_string()),
                        claim_ttl: StdDuration::from_secs(30),
                        drain_timeout: StdDuration::from_secs(5),
                    },
                )
                .await
                .expect("start worker server");
                let registration = server.jobs().first().expect("job registration").clone();
                assert_eq!(registration.queue.as_deref(), Some("scan-jobs"));

                let event_log = server.event_log();
                let response_topic = harn_vm::worker_response_topic_name("scan-jobs");
                let topic = Topic::new(response_topic.clone()).expect("response topic");
                let latest = event_log.latest(&topic).await.expect("latest response");
                let mut responses = event_log
                    .clone()
                    .subscribe(&topic, latest)
                    .await
                    .expect("subscribe responses");

                let request = serde_json::json!({"repo": "burin-labs/harn"});
                let event = job_event("scan", request.clone()).expect("job event");
                WorkerQueue::new(event_log.clone())
                    .enqueue(&harn_vm::WorkerQueueJob {
                        queue: "scan-jobs".to_string(),
                        trigger_id: registration.binding_id.clone(),
                        binding_key: registration.binding_key.clone(),
                        binding_version: registration.binding_version,
                        event,
                        replay_of_event_id: None,
                        priority: WorkerQueuePriority::Normal,
                    })
                    .await
                    .expect("enqueue job");

                let response = tokio::time::timeout(StdDuration::from_secs(5), async {
                    loop {
                        let Some(received) = responses.next().await else {
                            panic!("response stream ended");
                        };
                        let (_, event) = received.expect("response event");
                        if event.kind != "job_response" {
                            continue;
                        }
                        return serde_json::from_value::<WorkerQueueResponseRecord>(event.payload)
                            .expect("response record");
                    }
                })
                .await
                .expect("worker response");

                let outcome = response.outcome.expect("dispatch outcome");
                assert_eq!(outcome.status, DispatchStatus::Succeeded);
                assert_eq!(outcome.result.expect("result")["echo"], request);
                assert_eq!(response.error, None);

                let report = server.shutdown().await.expect("shutdown worker");
                assert!(report.drained);
                assert_eq!(report.queues, 1);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_job_names_are_rejected() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
@job("scan")
pub fn scan_a() -> dict { return {} }

@job("scan")
pub fn scan_b() -> dict { return {} }
"#,
                )
                .await;

                let error = run_job_once(&script, "scan", serde_json::json!({}))
                    .await
                    .expect_err("duplicate job name");
                assert!(error.message().contains("job names must be unique"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_from_files_writes_result_out() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan")
pub fn scan(event: TriggerEvent) -> dict {
  return {status: "ok", echo: event.provider_payload.raw}
}
"#,
                )
                .await;
                let request_path = dir.path().join("req.json");
                tokio::fs::write(&request_path, r#"{"k": "v"}"#)
                    .await
                    .expect("write request");
                let out_path = dir.path().join("out.json");

                let (outcome, rendered) =
                    run_job_from_files(&script, "scan", &request_path, Some(&out_path), false)
                        .await
                        .expect("run job from files");

                assert!(outcome.succeeded());
                let written = tokio::fs::read_to_string(&out_path)
                    .await
                    .expect("read out");
                assert_eq!(written, rendered);
                let parsed: serde_json::Value =
                    serde_json::from_str(&written).expect("parse report");
                assert_eq!(parsed["echo"], serde_json::json!({"k": "v"}));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_job_attribute_is_an_error() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r"
pub fn scan(req: dict) -> dict { return req }
",
                )
                .await;
                let error = run_job_once(&script, "scan", serde_json::json!({}))
                    .await
                    .expect_err("not a job");
                assert!(error.message().contains("no `@job(\"scan\")`"));
            })
            .await;
    }
}
