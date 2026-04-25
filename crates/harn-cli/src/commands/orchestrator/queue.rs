use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use serde::Serialize;

use crate::cli::{
    OrchestratorQueueArgs, OrchestratorQueueCommand, OrchestratorQueueDrainArgs,
    OrchestratorQueueLsArgs, OrchestratorQueuePurgeArgs,
};

use super::common::{
    format_duration, format_timestamp, load_local_runtime, read_topic, stranded_envelopes,
    StrandedEnvelopeRecord, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_INBOX_CLAIMS_TOPIC,
    TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_INBOX_LEGACY_TOPIC, TRIGGER_OUTBOX_TOPIC,
};

pub(super) async fn run(args: OrchestratorQueueArgs) -> Result<(), String> {
    match args.command.unwrap_or(OrchestratorQueueCommand::Ls(
        OrchestratorQueueLsArgs::default(),
    )) {
        OrchestratorQueueCommand::Ls(ls) => run_ls(args.local, ls).await,
        OrchestratorQueueCommand::Drain(drain) => run_drain(args.local, drain).await,
        OrchestratorQueueCommand::Purge(purge) => run_purge(args.local, purge).await,
    }
}

async fn run_ls(
    local: crate::cli::OrchestratorLocalArgs,
    args: OrchestratorQueueLsArgs,
) -> Result<(), String> {
    let ctx = load_local_runtime(&local).await?;
    let overview = build_overview(&ctx.event_log).await?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&overview).map_err(|error| error.to_string())?
        );
        return Ok(());
    }

    println!("Queue:");
    println!(
        "- dispatcher_in_flight={}",
        overview.dispatcher.dispatcher_in_flight
    );
    println!(
        "- dispatcher_retry_queue_depth={}",
        overview.dispatcher.dispatcher_retry_queue_depth
    );
    println!(
        "- inferred_in_flight={}",
        overview.dispatcher.inferred_in_flight
    );
    println!(
        "- inferred_pending_retries={}",
        overview.dispatcher.inferred_pending_retries
    );
    println!(
        "- stranded_envelopes={}",
        overview.dispatcher.stranded_envelopes.len()
    );
    println!(
        "- inbox_claims_written={}",
        overview.dispatcher.inbox_claims_written
    );
    println!(
        "- inbox_envelopes_written={}",
        overview.dispatcher.inbox_envelopes_written
    );
    println!(
        "- inbox_duplicates_rejected={}",
        overview.dispatcher.inbox_duplicates_rejected
    );
    println!(
        "- inbox_fast_path_hits={}",
        overview.dispatcher.inbox_fast_path_hits
    );
    println!(
        "- inbox_durable_hits={}",
        overview.dispatcher.inbox_durable_hits
    );
    println!(
        "- inbox_expired_entries={}",
        overview.dispatcher.inbox_expired_entries
    );
    println!(
        "- inbox_active_entries={}",
        overview.dispatcher.inbox_active_entries
    );

    println!();
    println!("Worker queues:");
    if overview.worker_queues.is_empty() {
        println!("- none");
    } else {
        for queue in &overview.worker_queues {
            let oldest = queue
                .oldest_unclaimed_age_ms
                .map(|age_ms| format_duration(StdDuration::from_millis(age_ms)))
                .unwrap_or_else(|| "-".to_string());
            println!(
                "- {} ready={} in_flight={} acked={} purged={} responses={} oldest_unclaimed_age={}",
                queue.queue,
                queue.ready,
                queue.in_flight,
                queue.acked,
                queue.purged,
                queue.responses,
                oldest,
            );
        }
    }

    println!();
    println!("In-flight dispatches:");
    if overview.dispatcher.in_flight_dispatches.is_empty() {
        println!("- none");
    } else {
        for key in &overview.dispatcher.in_flight_dispatches {
            println!("- {key}");
        }
    }

    println!();
    println!("Pending retries:");
    if overview.dispatcher.pending_retries.is_empty() {
        println!("- none");
    } else {
        for key in &overview.dispatcher.pending_retries {
            println!("- {key}");
        }
    }

    println!();
    println!("Stranded envelopes:");
    render_stranded(&overview.dispatcher.stranded_envelopes);

    println!();
    println!(
        "Scheduler policy: strategy={} fairness_key={}",
        overview.scheduler.policy.strategy_name(),
        overview.scheduler.policy.fairness_key.as_str(),
    );
    if overview.scheduler.per_queue.is_empty() {
        println!("- no queues observed yet");
    } else {
        for queue in &overview.scheduler.per_queue {
            println!(
                "- queue={} rounds_completed={} starvation_promotions={}",
                queue.queue, queue.rounds_completed, queue.starvation_promotions_total,
            );
            if queue.keys.is_empty() {
                println!("    (no fairness keys observed)");
            } else {
                for stat in &queue.keys {
                    let oldest = if stat.oldest_ready_age_ms > 0 {
                        format_duration(StdDuration::from_millis(stat.oldest_ready_age_ms))
                    } else {
                        "-".to_string()
                    };
                    println!(
                        "    key={} weight={} ready={} in_flight={} deficit={} selected={} deferred={} oldest_eligible={}",
                        stat.fairness_key,
                        stat.weight,
                        stat.ready_jobs,
                        stat.in_flight,
                        stat.deficit,
                        stat.selected_total,
                        stat.deferred_total,
                        oldest,
                    );
                }
            }
        }
    }
    Ok(())
}

async fn run_drain(
    local: crate::cli::OrchestratorLocalArgs,
    args: OrchestratorQueueDrainArgs,
) -> Result<(), String> {
    let ctx = load_local_runtime(&local).await?;
    let queue = harn_vm::WorkerQueue::new(ctx.event_log.clone());
    let dispatcher = harn_vm::Dispatcher::with_event_log(ctx.vm, ctx.event_log.clone());
    let consumer_id = args.consumer_id.unwrap_or_else(default_consumer_id);
    let mut drained = Vec::new();
    let mut acked = 0usize;
    let mut deferred = 0usize;

    loop {
        let Some(claimed) = queue
            .claim_next(&args.queue, &consumer_id, args.claim_ttl)
            .await
            .map_err(|error| format!("failed to claim worker job: {error}"))?
        else {
            break;
        };

        let heartbeat =
            start_claim_heartbeat(queue.clone(), claimed.handle.clone(), args.claim_ttl);
        let response = match harn_vm::resolve_live_trigger_binding(&claimed.job.trigger_id, None) {
            Ok(binding) if matches!(binding.handler, harn_vm::TriggerHandlerSpec::Worker { .. }) => {
                harn_vm::WorkerQueueResponseRecord {
                    queue: args.queue.clone(),
                    job_event_id: claimed.handle.job_event_id,
                    consumer_id: consumer_id.clone(),
                    handled_at_ms: now_ms(),
                    outcome: None,
                    error: Some(format!(
                        "worker queue '{}' resolved trigger '{}' to another worker:// handler; queue drains require a non-worker consumer binding",
                        args.queue, claimed.job.trigger_id
                    )),
                }
            }
            Ok(binding) => match dispatcher.dispatch(&binding, claimed.job.event.clone()).await {
                Ok(outcome) => harn_vm::WorkerQueueResponseRecord {
                    queue: args.queue.clone(),
                    job_event_id: claimed.handle.job_event_id,
                    consumer_id: consumer_id.clone(),
                    handled_at_ms: now_ms(),
                    outcome: Some(outcome),
                    error: None,
                },
                Err(error) => harn_vm::WorkerQueueResponseRecord {
                    queue: args.queue.clone(),
                    job_event_id: claimed.handle.job_event_id,
                    consumer_id: consumer_id.clone(),
                    handled_at_ms: now_ms(),
                    outcome: None,
                    error: Some(error.to_string()),
                },
            },
            Err(error) => harn_vm::WorkerQueueResponseRecord {
                queue: args.queue.clone(),
                job_event_id: claimed.handle.job_event_id,
                consumer_id: consumer_id.clone(),
                handled_at_ms: now_ms(),
                outcome: None,
                error: Some(format!(
                    "failed to resolve consumer binding '{}': {error}",
                    claimed.job.trigger_id
                )),
            },
        };

        stop_claim_heartbeat(heartbeat).await;
        queue
            .append_response(&args.queue, &response)
            .await
            .map_err(|error| format!("failed to append worker response: {error}"))?;
        let should_ack = response.error.is_none()
            && response.outcome.as_ref().is_some_and(|outcome| {
                matches!(
                    outcome.status,
                    harn_vm::DispatchStatus::Succeeded | harn_vm::DispatchStatus::Skipped
                )
            });
        if should_ack {
            queue
                .ack_claim(&claimed.handle)
                .await
                .map_err(|error| format!("failed to ack worker claim: {error}"))?;
            acked += 1;
        } else {
            deferred += 1;
        }
        drained.push(response);
    }

    let state = queue
        .queue_state(&args.queue)
        .await
        .map_err(|error| format!("failed to load worker queue state: {error}"))?;
    let result = QueueDrainResult {
        queue: args.queue,
        consumer_id,
        claim_ttl_ms: args.claim_ttl.as_millis() as u64,
        drained: drained.len(),
        acked,
        deferred,
        responses: drained,
        summary: state.summary(now_ms()),
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
        );
    } else {
        println!(
            "Drained queue '{}' as {}: jobs={} acked={} deferred={} ready={} in_flight={} responses={}",
            result.queue,
            result.consumer_id,
            result.drained,
            result.acked,
            result.deferred,
            result.summary.ready,
            result.summary.in_flight,
            result.summary.responses,
        );
        for response in &result.responses {
            let status = response
                .outcome
                .as_ref()
                .map(|outcome| outcome.status.as_str())
                .unwrap_or("error");
            println!(
                "- job_event_id={} status={} trigger_id={} error={}",
                response.job_event_id,
                status,
                response
                    .outcome
                    .as_ref()
                    .map(|outcome| outcome.trigger_id.as_str())
                    .unwrap_or("-"),
                response.error.as_deref().unwrap_or("-"),
            );
        }
    }
    Ok(())
}

async fn run_purge(
    local: crate::cli::OrchestratorLocalArgs,
    args: OrchestratorQueuePurgeArgs,
) -> Result<(), String> {
    if !args.confirm {
        return Err(
            "queue purge is destructive; rerun with `--confirm` to drop ready jobs".to_string(),
        );
    }
    let ctx = load_local_runtime(&local).await?;
    let queue = harn_vm::WorkerQueue::new(ctx.event_log.clone());
    let purged = queue
        .purge_unclaimed(
            &args.queue,
            &default_consumer_id(),
            Some("manual purge via harn orchestrator queue purge"),
        )
        .await
        .map_err(|error| format!("failed to purge worker queue: {error}"))?;
    let state = queue
        .queue_state(&args.queue)
        .await
        .map_err(|error| format!("failed to load worker queue state: {error}"))?;
    let result = QueuePurgeResult {
        queue: args.queue,
        purged,
        summary: state.summary(now_ms()),
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&result).map_err(|error| error.to_string())?
        );
    } else {
        println!(
            "Purged queue '{}': purged={} ready={} in_flight={} purged_total={}",
            result.queue,
            result.purged,
            result.summary.ready,
            result.summary.in_flight,
            result.summary.purged,
        );
    }
    Ok(())
}

async fn build_overview(
    event_log: &Arc<harn_vm::event_log::AnyEventLog>,
) -> Result<QueueOverview, String> {
    let dispatcher = harn_vm::snapshot_dispatcher_stats();
    let outbox = read_topic(event_log, TRIGGER_OUTBOX_TOPIC).await?;
    let attempts = read_topic(event_log, TRIGGER_ATTEMPTS_TOPIC).await?;
    let claim_events = read_topic(event_log, TRIGGER_INBOX_CLAIMS_TOPIC).await?;
    let envelope_events = read_topic(event_log, TRIGGER_INBOX_ENVELOPES_TOPIC).await?;
    let legacy_inbox_events = read_topic(event_log, TRIGGER_INBOX_LEGACY_TOPIC).await?;
    let stranded = stranded_envelopes(event_log, StdDuration::ZERO).await?;

    let mut started = BTreeSet::new();
    let mut finished = BTreeSet::new();
    for (_, event) in &outbox {
        let Some(key) = dispatch_key(&event.headers) else {
            continue;
        };
        match event.kind.as_str() {
            "dispatch_started" => {
                started.insert(key);
            }
            "dispatch_succeeded" | "dispatch_failed" => {
                finished.insert(key);
            }
            _ => {}
        }
    }
    let in_flight: Vec<_> = started.difference(&finished).cloned().collect();

    let mut scheduled = BTreeSet::new();
    for (_, event) in &attempts {
        if event.kind != "retry_scheduled" {
            continue;
        }
        if let Some(key) = dispatch_key(&event.headers) {
            scheduled.insert(key);
        }
    }
    let pending_retries: Vec<_> = scheduled.difference(&started).cloned().collect();

    let inbox_metrics = Arc::new(harn_vm::MetricsRegistry::default());
    let _inbox = harn_vm::triggers::InboxIndex::new(event_log.clone(), inbox_metrics.clone())
        .await
        .map_err(|error| error.to_string())?;
    let inbox_snapshot = inbox_metrics.snapshot();
    let inbox_claims_written = claim_events
        .iter()
        .chain(legacy_inbox_events.iter())
        .filter(|(_, event)| event.kind == "dedupe_claim")
        .count();
    let inbox_envelopes_written = envelope_events
        .iter()
        .chain(legacy_inbox_events.iter())
        .filter(|(_, event)| event.kind == "event_ingested")
        .count();

    let worker_queue = harn_vm::WorkerQueue::new(event_log.clone());
    let worker_queue_inspections = worker_queue
        .inspect_all_queues()
        .await
        .map_err(|error| error.to_string())?;
    let worker_queues = worker_queue_inspections
        .iter()
        .map(|snap| snap.summary.clone())
        .collect();
    let scheduler_overview = SchedulerOverview {
        policy: worker_queue.policy(),
        per_queue: worker_queue_inspections
            .into_iter()
            .map(|snap| SchedulerQueueOverview {
                queue: snap.summary.queue.clone(),
                strategy: snap.scheduler.strategy,
                fairness_key: snap.scheduler.fairness_key,
                rounds_completed: snap.scheduler.rounds_completed,
                starvation_promotions_total: snap.scheduler.starvation_promotions_total,
                keys: snap.scheduler.keys,
            })
            .collect(),
    };

    Ok(QueueOverview {
        dispatcher: DispatcherQueueOverview {
            dispatcher_in_flight: dispatcher.in_flight,
            dispatcher_retry_queue_depth: dispatcher.retry_queue_depth,
            inferred_in_flight: in_flight.len(),
            inferred_pending_retries: pending_retries.len(),
            inbox_claims_written,
            inbox_envelopes_written,
            inbox_duplicates_rejected: inbox_snapshot.inbox_duplicates_rejected,
            inbox_fast_path_hits: inbox_snapshot.inbox_fast_path_hits,
            inbox_durable_hits: inbox_snapshot.inbox_durable_hits,
            inbox_expired_entries: inbox_snapshot.inbox_expired_entries,
            inbox_active_entries: inbox_snapshot.inbox_active_entries as usize,
            in_flight_dispatches: in_flight,
            pending_retries,
            stranded_envelopes: stranded,
        },
        worker_queues,
        scheduler: scheduler_overview,
    })
}

fn render_stranded(stranded: &[StrandedEnvelopeRecord]) {
    if stranded.is_empty() {
        println!("- none");
        return;
    }
    for envelope in stranded {
        println!(
            "- event_id={} trigger_id={} binding_version={} provider={} kind={} age={} received_at={} inbox_offset={}",
            envelope.event_id,
            envelope.trigger_id.as_deref().unwrap_or("-"),
            envelope
                .binding_version
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            envelope.provider,
            envelope.kind,
            format_duration(envelope.age),
            format_timestamp(envelope.received_at),
            envelope.inbox_offset,
        );
    }
}

fn dispatch_key(headers: &std::collections::BTreeMap<String, String>) -> Option<String> {
    let binding_key = headers.get("binding_key")?;
    let event_id = headers.get("event_id")?;
    let attempt = headers.get("attempt")?;
    Some(format!("{binding_key}:{event_id}:{attempt}"))
}

fn default_consumer_id() -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "local".to_string());
    format!("{}-pid{}-{}", host, std::process::id(), now_ms())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn heartbeat_interval(ttl: StdDuration) -> StdDuration {
    let millis = ttl.as_millis() as u64;
    if millis <= 1 {
        StdDuration::from_millis(1)
    } else {
        StdDuration::from_millis((millis / 2).max(1))
    }
}

fn start_claim_heartbeat(
    queue: harn_vm::WorkerQueue,
    handle: harn_vm::WorkerQueueClaimHandle,
    ttl: StdDuration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(heartbeat_interval(ttl)).await;
            let still_owned = queue.renew_claim(&handle, ttl).await.unwrap_or(false);
            if !still_owned {
                break;
            }
        }
    })
}

async fn stop_claim_heartbeat(handle: tokio::task::JoinHandle<()>) {
    handle.abort();
    let _ = handle.await;
}

#[derive(Clone, Debug, Serialize)]
struct QueueOverview {
    dispatcher: DispatcherQueueOverview,
    worker_queues: Vec<harn_vm::WorkerQueueSummary>,
    scheduler: SchedulerOverview,
}

#[derive(Clone, Debug, Serialize)]
struct SchedulerOverview {
    policy: harn_vm::SchedulerPolicy,
    per_queue: Vec<SchedulerQueueOverview>,
}

#[derive(Clone, Debug, Serialize)]
struct SchedulerQueueOverview {
    queue: String,
    strategy: String,
    fairness_key: String,
    rounds_completed: u64,
    starvation_promotions_total: u64,
    keys: Vec<harn_vm::SchedulerKeyStat>,
}

#[derive(Clone, Debug, Serialize)]
struct DispatcherQueueOverview {
    dispatcher_in_flight: u64,
    dispatcher_retry_queue_depth: u64,
    inferred_in_flight: usize,
    inferred_pending_retries: usize,
    inbox_claims_written: usize,
    inbox_envelopes_written: usize,
    inbox_duplicates_rejected: u64,
    inbox_fast_path_hits: u64,
    inbox_durable_hits: u64,
    inbox_expired_entries: u64,
    inbox_active_entries: usize,
    in_flight_dispatches: Vec<String>,
    pending_retries: Vec<String>,
    stranded_envelopes: Vec<StrandedEnvelopeRecord>,
}

#[derive(Clone, Debug, Serialize)]
struct QueueDrainResult {
    queue: String,
    consumer_id: String,
    claim_ttl_ms: u64,
    drained: usize,
    acked: usize,
    deferred: usize,
    responses: Vec<harn_vm::WorkerQueueResponseRecord>,
    summary: harn_vm::WorkerQueueSummary,
}

#[derive(Clone, Debug, Serialize)]
struct QueuePurgeResult {
    queue: String,
    purged: usize,
    summary: harn_vm::WorkerQueueSummary,
}
