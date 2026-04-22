use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::cli::OrchestratorStatsArgs;

use super::common::{
    format_duration, load_local_runtime, print_json, read_topic, TRIGGERS_LIFECYCLE_TOPIC,
    TRIGGER_ATTEMPTS_TOPIC, TRIGGER_DLQ_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC,
    TRIGGER_INBOX_LEGACY_TOPIC,
};

const LLM_TRANSCRIPT_TOPIC: &str = "agent.transcript.llm";
const STATS_TOPIC: &str = "orchestrator.analytics.stats";

#[derive(Clone, Debug, Default, Serialize)]
struct OrchestratorStatsPayload {
    generated_at: String,
    window_seconds: u64,
    since_ms: i64,
    top_n: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant: Option<String>,
    totals: StatsTotals,
    triggers: Vec<TriggerStats>,
    providers: Vec<ProviderStats>,
    pipelines: Vec<PipelineCostStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persisted_event_id: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct StatsTotals {
    fire_count: u64,
    dispatch_successes: u64,
    dispatch_failures: u64,
    dispatch_skipped: u64,
    dlq_count: u64,
    predicate_evaluations: u64,
    predicate_misses: u64,
    predicate_miss_rate: f64,
    handler_attempts: u64,
    llm_call_count: u64,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
    cost_routing_savings_usd: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
struct TriggerStats {
    trigger_id: String,
    fire_count: u64,
    dispatch_successes: u64,
    dispatch_failures: u64,
    dispatch_skipped: u64,
    dlq_count: u64,
    dlq_rate: f64,
    predicate_evaluations: u64,
    predicate_misses: u64,
    predicate_miss_rate: f64,
    handler_attempts: u64,
    median_handler_duration_ms: Option<u64>,
    p99_handler_duration_ms: Option<u64>,
    llm_call_count: u64,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
    handlers: Vec<HandlerStats>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct HandlerStats {
    handler: String,
    attempts: u64,
    dispatch_successes: u64,
    dispatch_failures: u64,
    dlq_count: u64,
    dlq_rate: f64,
    median_duration_ms: Option<u64>,
    p99_duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct ProviderStats {
    provider: String,
    call_count: u64,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
struct PipelineCostStats {
    pipeline: String,
    call_count: u64,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
    cost_routing_savings_usd: f64,
}

#[derive(Clone, Debug, Default)]
struct EventContext {
    trigger_id: String,
    tenant_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct TriggerAccumulator {
    fire_count: u64,
    dispatch_successes: u64,
    dispatch_failures: u64,
    dispatch_skipped: u64,
    dlq_count: u64,
    predicate_evaluations: u64,
    predicate_misses: u64,
    handler_attempts: u64,
    llm_call_count: u64,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
    handler_durations_ms: Vec<u64>,
    handlers: BTreeMap<String, HandlerAccumulator>,
}

#[derive(Clone, Debug, Default)]
struct HandlerAccumulator {
    attempts: u64,
    dispatch_successes: u64,
    dispatch_failures: u64,
    dlq_count: u64,
    durations_ms: Vec<u64>,
}

#[derive(Clone, Debug, Default)]
struct ProviderAccumulator {
    call_count: u64,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
}

pub(super) async fn run(args: OrchestratorStatsArgs) -> Result<(), String> {
    let ctx = load_local_runtime(&args.local).await?;
    let top_n = args.top.max(1);
    let mut payload =
        collect_stats(&ctx.event_log, args.window, top_n, args.tenant.clone()).await?;
    payload.persisted_event_id = Some(persist_stats_snapshot(&ctx.event_log, &payload).await?);

    if args.json {
        return print_json(&payload);
    }

    println!("Orchestrator stats:");
    println!("- window={}", format_duration(args.window));
    if let Some(tenant) = args.tenant.as_deref() {
        println!("- tenant={tenant}");
    }
    println!("- fire_count={}", payload.totals.fire_count);
    println!(
        "- predicates={} misses={} miss_rate={:.2}%",
        payload.totals.predicate_evaluations,
        payload.totals.predicate_misses,
        payload.totals.predicate_miss_rate * 100.0
    );
    println!(
        "- dispatches succeeded:{} failed:{} skipped:{} dlq:{}",
        payload.totals.dispatch_successes,
        payload.totals.dispatch_failures,
        payload.totals.dispatch_skipped,
        payload.totals.dlq_count
    );
    println!(
        "- llm_calls={} input_tokens={} output_tokens={} cost=${:.6}",
        payload.totals.llm_call_count,
        payload.totals.input_tokens,
        payload.totals.output_tokens,
        payload.totals.cost_usd
    );
    println!(
        "- persisted_snapshot={}",
        payload.persisted_event_id.unwrap_or_default()
    );

    println!();
    println!("Hot triggers:");
    if payload.triggers.is_empty() {
        println!("- none");
    } else {
        for trigger in &payload.triggers {
            println!(
                "- {} fires={} miss_rate={:.2}% dlq_rate={:.2}% median={} p99={} cost=${:.6}",
                trigger.trigger_id,
                trigger.fire_count,
                trigger.predicate_miss_rate * 100.0,
                trigger.dlq_rate * 100.0,
                format_optional_ms(trigger.median_handler_duration_ms),
                format_optional_ms(trigger.p99_handler_duration_ms),
                trigger.cost_usd
            );
        }
    }

    println!();
    println!("Providers:");
    if payload.providers.is_empty() {
        println!("- none");
    } else {
        for provider in &payload.providers {
            println!(
                "- {} calls={} input_tokens={} output_tokens={} cost=${:.6}",
                provider.provider,
                provider.call_count,
                provider.input_tokens,
                provider.output_tokens,
                provider.cost_usd
            );
        }
    }

    Ok(())
}

async fn collect_stats(
    log: &Arc<AnyEventLog>,
    window: StdDuration,
    top_n: usize,
    tenant: Option<String>,
) -> Result<OrchestratorStatsPayload, String> {
    let generated_at = OffsetDateTime::now_utc();
    let since_ms = generated_at
        .unix_timestamp()
        .saturating_mul(1000)
        .saturating_sub(i64::try_from(window.as_millis()).unwrap_or(i64::MAX));

    let mut by_event_id = BTreeMap::<String, EventContext>::new();
    let mut by_binding_key = BTreeMap::<String, EventContext>::new();
    let mut triggers = BTreeMap::<String, TriggerAccumulator>::new();
    let mut providers = BTreeMap::<String, ProviderAccumulator>::new();
    let mut pipelines = BTreeMap::<String, ProviderAccumulator>::new();

    let mut inbox = read_topic(log, TRIGGER_INBOX_ENVELOPES_TOPIC).await?;
    inbox.extend(read_topic(log, TRIGGER_INBOX_LEGACY_TOPIC).await?);
    for (_, event) in inbox {
        if event.kind != "event_ingested" || event.occurred_at_ms < since_ms {
            continue;
        }
        let envelope: harn_vm::triggers::dispatcher::InboxEnvelope =
            match serde_json::from_value(event.payload) {
                Ok(envelope) => envelope,
                Err(_) => continue,
            };
        if tenant_filter_mismatch(
            &tenant,
            envelope.event.tenant_id.as_ref().map(|id| id.0.as_str()),
        ) {
            continue;
        }
        let trigger_id = envelope
            .trigger_id
            .clone()
            .unwrap_or_else(|| "unmatched".to_string());
        let binding_key = envelope
            .binding_version
            .map(|version| format!("{trigger_id}@v{version}"));
        let context = EventContext {
            trigger_id: trigger_id.clone(),
            tenant_id: envelope.event.tenant_id.as_ref().map(|id| id.0.clone()),
        };
        by_event_id.insert(envelope.event.id.0.clone(), context.clone());
        if let Some(binding_key) = binding_key {
            by_binding_key.insert(binding_key, context);
        }
        triggers.entry(trigger_id).or_default().fire_count += 1;
    }

    let lifecycle = read_topic(log, TRIGGERS_LIFECYCLE_TOPIC).await?;
    for (_, event) in lifecycle {
        if event.kind != "predicate.evaluated" || event.occurred_at_ms < since_ms {
            continue;
        }
        let context = context_for_event(&event, &by_event_id, &by_binding_key);
        if tenant_filter_mismatch(
            &tenant,
            context.as_ref().and_then(|ctx| ctx.tenant_id.as_deref()),
        ) {
            continue;
        }
        let trigger_id = trigger_id_for(&event, context.as_ref());
        let trigger = triggers.entry(trigger_id).or_default();
        trigger.predicate_evaluations += 1;
        if !event
            .payload
            .get("result")
            .and_then(|value| value.as_bool())
            .unwrap_or(true)
        {
            trigger.predicate_misses += 1;
        }
        trigger.cost_usd += event
            .payload
            .get("cost_usd")
            .and_then(|value| value.as_f64())
            .unwrap_or_default();
        trigger.input_tokens += event
            .payload
            .get("input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default();
        trigger.output_tokens += event
            .payload
            .get("output_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default();
    }

    let outbox = read_topic(log, harn_vm::TRIGGER_OUTBOX_TOPIC).await?;
    for (_, event) in outbox {
        if event.occurred_at_ms < since_ms {
            continue;
        }
        let context = context_for_event(&event, &by_event_id, &by_binding_key);
        if tenant_filter_mismatch(
            &tenant,
            context.as_ref().and_then(|ctx| ctx.tenant_id.as_deref()),
        ) {
            continue;
        }
        let trigger_id = trigger_id_for(&event, context.as_ref());
        let trigger = triggers.entry(trigger_id).or_default();
        match event.kind.as_str() {
            "dispatch_succeeded" => trigger.dispatch_successes += 1,
            "dispatch_failed" => trigger.dispatch_failures += 1,
            "dispatch_skipped" => trigger.dispatch_skipped += 1,
            _ => {}
        }
    }

    let attempts = read_topic(log, TRIGGER_ATTEMPTS_TOPIC).await?;
    for (_, event) in attempts {
        if event.kind != "attempt_recorded" || event.occurred_at_ms < since_ms {
            continue;
        }
        let attempt: harn_vm::triggers::dispatcher::DispatchAttemptRecord =
            match serde_json::from_value(event.payload.clone()) {
                Ok(attempt) => attempt,
                Err(_) => continue,
            };
        let context = context_for_event(&event, &by_event_id, &by_binding_key);
        if tenant_filter_mismatch(
            &tenant,
            context.as_ref().and_then(|ctx| ctx.tenant_id.as_deref()),
        ) {
            continue;
        }
        let trigger_id = if attempt.trigger_id.is_empty() {
            trigger_id_for(&event, context.as_ref())
        } else {
            attempt.trigger_id.clone()
        };
        let duration_ms = attempt_duration_ms(&attempt);
        let trigger = triggers.entry(trigger_id).or_default();
        trigger.handler_attempts += 1;
        if let Some(duration_ms) = duration_ms {
            trigger.handler_durations_ms.push(duration_ms);
        }
        let handler = trigger
            .handlers
            .entry(attempt.handler_kind.clone())
            .or_default();
        handler.attempts += 1;
        if attempt.outcome == "success" {
            handler.dispatch_successes += 1;
        } else {
            handler.dispatch_failures += 1;
        }
        if let Some(duration_ms) = duration_ms {
            handler.durations_ms.push(duration_ms);
        }
    }

    let dlq = read_topic(log, TRIGGER_DLQ_TOPIC).await?;
    for (_, event) in dlq {
        if !matches!(event.kind.as_str(), "dlq_moved" | "dlq_entry")
            || event.occurred_at_ms < since_ms
        {
            continue;
        }
        let context = context_for_event(&event, &by_event_id, &by_binding_key);
        if tenant_filter_mismatch(
            &tenant,
            context.as_ref().and_then(|ctx| ctx.tenant_id.as_deref()),
        ) {
            continue;
        }
        let trigger_id = trigger_id_for(&event, context.as_ref());
        let handler_kind = event
            .payload
            .get("attempts")
            .and_then(|value| value.as_array())
            .and_then(|attempts| attempts.last())
            .and_then(|attempt| attempt.get("handler_kind"))
            .and_then(|value| value.as_str())
            .unwrap_or("unknown")
            .to_string();
        let trigger = triggers.entry(trigger_id).or_default();
        trigger.dlq_count += 1;
        trigger.handlers.entry(handler_kind).or_default().dlq_count += 1;
    }

    let llm_events = read_topic(log, LLM_TRANSCRIPT_TOPIC).await?;
    for (_, event) in llm_events {
        if event.kind != "provider_call_response" || event.occurred_at_ms < since_ms {
            continue;
        }
        if tenant_filter_mismatch(&tenant, event.headers.get("tenant_id").map(String::as_str)) {
            continue;
        }
        let provider = event
            .payload
            .get("provider")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown")
            .to_string();
        let trigger_id = event
            .headers
            .get("trigger_id")
            .cloned()
            .unwrap_or_else(|| "standalone".to_string());
        let pipeline = event
            .headers
            .get("pipeline")
            .cloned()
            .unwrap_or_else(|| trigger_id.clone());
        let input_tokens = event
            .payload
            .get("input_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default();
        let output_tokens = event
            .payload
            .get("output_tokens")
            .and_then(|value| value.as_i64())
            .unwrap_or_default();
        let cost_usd = event
            .payload
            .get("cost_usd")
            .and_then(|value| value.as_f64())
            .unwrap_or_default();

        let trigger = triggers.entry(trigger_id).or_default();
        trigger.llm_call_count += 1;
        trigger.input_tokens += input_tokens;
        trigger.output_tokens += output_tokens;
        trigger.cost_usd += cost_usd;

        add_provider_sample(
            providers.entry(provider).or_default(),
            input_tokens,
            output_tokens,
            cost_usd,
        );
        add_provider_sample(
            pipelines.entry(pipeline).or_default(),
            input_tokens,
            output_tokens,
            cost_usd,
        );
    }

    let mut trigger_stats: Vec<_> = triggers
        .into_iter()
        .map(|(trigger_id, acc)| build_trigger_stats(trigger_id, acc))
        .collect();
    let mut provider_stats: Vec<_> = providers
        .into_iter()
        .map(|(provider, acc)| ProviderStats {
            provider,
            call_count: acc.call_count,
            input_tokens: acc.input_tokens,
            output_tokens: acc.output_tokens,
            cost_usd: acc.cost_usd,
        })
        .collect();
    let mut totals = build_totals(&trigger_stats, &provider_stats);

    trigger_stats.sort_by(|left, right| {
        right
            .fire_count
            .cmp(&left.fire_count)
            .then_with(|| right.cost_usd.total_cmp(&left.cost_usd))
            .then_with(|| left.trigger_id.cmp(&right.trigger_id))
    });
    trigger_stats.truncate(top_n);

    provider_stats.sort_by(|left, right| {
        right
            .cost_usd
            .total_cmp(&left.cost_usd)
            .then_with(|| right.call_count.cmp(&left.call_count))
            .then_with(|| left.provider.cmp(&right.provider))
    });
    provider_stats.truncate(top_n);

    let mut pipeline_stats: Vec<_> = pipelines
        .into_iter()
        .map(|(pipeline, acc)| PipelineCostStats {
            pipeline,
            call_count: acc.call_count,
            input_tokens: acc.input_tokens,
            output_tokens: acc.output_tokens,
            cost_usd: acc.cost_usd,
            cost_routing_savings_usd: 0.0,
        })
        .collect();
    pipeline_stats.sort_by(|left, right| {
        right
            .cost_usd
            .total_cmp(&left.cost_usd)
            .then_with(|| right.call_count.cmp(&left.call_count))
            .then_with(|| left.pipeline.cmp(&right.pipeline))
    });
    pipeline_stats.truncate(top_n);

    totals.cost_routing_savings_usd = pipeline_stats
        .iter()
        .map(|pipeline| pipeline.cost_routing_savings_usd)
        .sum();
    Ok(OrchestratorStatsPayload {
        generated_at: generated_at
            .format(&Rfc3339)
            .unwrap_or_else(|_| generated_at.to_string()),
        window_seconds: window.as_secs(),
        since_ms,
        top_n,
        tenant,
        totals,
        triggers: trigger_stats,
        providers: provider_stats,
        pipelines: pipeline_stats,
        persisted_event_id: None,
    })
}

async fn persist_stats_snapshot(
    log: &Arc<AnyEventLog>,
    payload: &OrchestratorStatsPayload,
) -> Result<u64, String> {
    let topic = Topic::new(STATS_TOPIC).map_err(|error| error.to_string())?;
    let mut headers = BTreeMap::new();
    headers.insert(
        "window_seconds".to_string(),
        payload.window_seconds.to_string(),
    );
    if let Some(tenant) = payload.tenant.as_ref() {
        headers.insert("tenant_id".to_string(), tenant.clone());
    }
    let payload = serde_json::to_value(payload).map_err(|error| error.to_string())?;
    let event = LogEvent::new("stats_snapshot", payload).with_headers(headers);
    let id = log
        .append(&topic, event)
        .await
        .map_err(|error| error.to_string())?;
    log.flush().await.map_err(|error| error.to_string())?;
    Ok(id)
}

fn add_provider_sample(
    acc: &mut ProviderAccumulator,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
) {
    acc.call_count += 1;
    acc.input_tokens += input_tokens;
    acc.output_tokens += output_tokens;
    acc.cost_usd += cost_usd;
}

fn build_trigger_stats(trigger_id: String, mut acc: TriggerAccumulator) -> TriggerStats {
    let handlers = acc
        .handlers
        .into_iter()
        .map(|(handler, mut handler_acc)| {
            handler_acc.durations_ms.sort_unstable();
            HandlerStats {
                handler,
                attempts: handler_acc.attempts,
                dispatch_successes: handler_acc.dispatch_successes,
                dispatch_failures: handler_acc.dispatch_failures,
                dlq_count: handler_acc.dlq_count,
                dlq_rate: ratio(handler_acc.dlq_count, handler_acc.attempts),
                median_duration_ms: percentile(&handler_acc.durations_ms, 0.50),
                p99_duration_ms: percentile(&handler_acc.durations_ms, 0.99),
            }
        })
        .collect();

    acc.handler_durations_ms.sort_unstable();
    TriggerStats {
        trigger_id,
        fire_count: acc.fire_count,
        dispatch_successes: acc.dispatch_successes,
        dispatch_failures: acc.dispatch_failures,
        dispatch_skipped: acc.dispatch_skipped,
        dlq_count: acc.dlq_count,
        dlq_rate: ratio(acc.dlq_count, acc.fire_count),
        predicate_evaluations: acc.predicate_evaluations,
        predicate_misses: acc.predicate_misses,
        predicate_miss_rate: ratio(acc.predicate_misses, acc.predicate_evaluations),
        handler_attempts: acc.handler_attempts,
        median_handler_duration_ms: percentile(&acc.handler_durations_ms, 0.50),
        p99_handler_duration_ms: percentile(&acc.handler_durations_ms, 0.99),
        llm_call_count: acc.llm_call_count,
        input_tokens: acc.input_tokens,
        output_tokens: acc.output_tokens,
        cost_usd: acc.cost_usd,
        handlers,
    }
}

fn build_totals(triggers: &[TriggerStats], providers: &[ProviderStats]) -> StatsTotals {
    let mut totals = StatsTotals::default();
    for trigger in triggers {
        totals.fire_count += trigger.fire_count;
        totals.dispatch_successes += trigger.dispatch_successes;
        totals.dispatch_failures += trigger.dispatch_failures;
        totals.dispatch_skipped += trigger.dispatch_skipped;
        totals.dlq_count += trigger.dlq_count;
        totals.predicate_evaluations += trigger.predicate_evaluations;
        totals.predicate_misses += trigger.predicate_misses;
        totals.handler_attempts += trigger.handler_attempts;
    }
    for provider in providers {
        totals.llm_call_count += provider.call_count;
        totals.input_tokens += provider.input_tokens;
        totals.output_tokens += provider.output_tokens;
        totals.cost_usd += provider.cost_usd;
    }
    totals.predicate_miss_rate = ratio(totals.predicate_misses, totals.predicate_evaluations);
    totals
}

fn context_for_event(
    event: &LogEvent,
    by_event_id: &BTreeMap<String, EventContext>,
    by_binding_key: &BTreeMap<String, EventContext>,
) -> Option<EventContext> {
    event
        .headers
        .get("event_id")
        .and_then(|event_id| by_event_id.get(event_id))
        .cloned()
        .or_else(|| {
            event
                .headers
                .get("binding_key")
                .and_then(|binding_key| by_binding_key.get(binding_key))
                .cloned()
        })
}

fn trigger_id_for(event: &LogEvent, context: Option<&EventContext>) -> String {
    event
        .headers
        .get("trigger_id")
        .cloned()
        .or_else(|| context.map(|ctx| ctx.trigger_id.clone()))
        .or_else(|| {
            event
                .payload
                .get("trigger_id")
                .and_then(|value| value.as_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn tenant_filter_mismatch(filter: &Option<String>, tenant: Option<&str>) -> bool {
    match filter {
        Some(filter) => tenant != Some(filter.as_str()),
        None => false,
    }
}

fn attempt_duration_ms(
    attempt: &harn_vm::triggers::dispatcher::DispatchAttemptRecord,
) -> Option<u64> {
    let started = OffsetDateTime::parse(&attempt.started_at, &Rfc3339).ok()?;
    let completed = OffsetDateTime::parse(&attempt.completed_at, &Rfc3339).ok()?;
    let delta = completed - started;
    u64::try_from(delta.whole_milliseconds()).ok()
}

fn percentile(sorted: &[u64], percentile: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((sorted.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted.get(rank).copied()
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn format_optional_ms(value: Option<u64>) -> String {
    value
        .map(|value| format!("{value}ms"))
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use harn_vm::event_log::{install_memory_for_current_thread, EventLog, LogEvent, Topic};
    use time::OffsetDateTime;

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn collect_stats_rolls_up_trigger_predicate_dispatch_dlq_and_llm_cost() {
        let log = install_memory_for_current_thread(128);
        let inbox = Topic::new(TRIGGER_INBOX_ENVELOPES_TOPIC).unwrap();
        let lifecycle = Topic::new(TRIGGERS_LIFECYCLE_TOPIC).unwrap();
        let attempts = Topic::new(TRIGGER_ATTEMPTS_TOPIC).unwrap();
        let dlq = Topic::new(TRIGGER_DLQ_TOPIC).unwrap();
        let llm = Topic::new(LLM_TRANSCRIPT_TOPIC).unwrap();

        let mut event = harn_vm::TriggerEvent::new(
            harn_vm::ProviderId::from("github"),
            "issues.opened",
            None,
            "dedupe-1",
            Some(harn_vm::TenantId::new("tenant-a")),
            BTreeMap::new(),
            harn_vm::ProviderPayload::Known(
                harn_vm::triggers::event::KnownProviderPayload::Webhook(
                    harn_vm::triggers::GenericWebhookPayload {
                        source: Some("test".to_string()),
                        content_type: Some("application/json".to_string()),
                        raw: serde_json::json!({"ok": true}),
                    },
                ),
            ),
            harn_vm::SignatureStatus::Unsigned,
        );
        event.id.0 = "evt-1".to_string();
        let envelope = harn_vm::triggers::dispatcher::InboxEnvelope {
            trigger_id: Some("triage".to_string()),
            binding_version: Some(1),
            event,
        };
        log.append(
            &inbox,
            LogEvent::new("event_ingested", serde_json::to_value(envelope).unwrap()),
        )
        .await
        .unwrap();
        let headers = BTreeMap::from([
            ("event_id".to_string(), "evt-1".to_string()),
            ("trigger_id".to_string(), "triage".to_string()),
            ("binding_key".to_string(), "triage@v1".to_string()),
        ]);
        log.append(
            &lifecycle,
            LogEvent::new(
                "predicate.evaluated",
                serde_json::json!({"result": false, "cost_usd": 0.25, "input_tokens": 10, "output_tokens": 5}),
            )
            .with_headers(headers.clone()),
        )
        .await
        .unwrap();
        let now = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();
        log.append(
            &attempts,
            LogEvent::new(
                "attempt_recorded",
                serde_json::json!({
                    "trigger_id": "triage",
                    "binding_key": "triage@v1",
                    "event_id": "evt-1",
                    "attempt": 1,
                    "handler_kind": "local",
                    "started_at": now,
                    "completed_at": now,
                    "outcome": "success",
                    "error_msg": null,
                }),
            )
            .with_headers(headers.clone()),
        )
        .await
        .unwrap();
        log.append(
            &dlq,
            LogEvent::new("dlq_moved", serde_json::json!({"attempts": []}))
                .with_headers(headers.clone()),
        )
        .await
        .unwrap();
        log.append(
            &llm,
            LogEvent::new(
                "provider_call_response",
                serde_json::json!({
                    "provider": "openai",
                    "model": "gpt-4o-mini",
                    "input_tokens": 100,
                    "output_tokens": 40,
                    "cost_usd": 0.000039,
                }),
            )
            .with_headers(BTreeMap::from([
                ("trigger_id".to_string(), "triage".to_string()),
                ("tenant_id".to_string(), "tenant-a".to_string()),
            ])),
        )
        .await
        .unwrap();

        let stats = collect_stats(
            &log,
            StdDuration::from_secs(24 * 60 * 60),
            10,
            Some("tenant-a".to_string()),
        )
        .await
        .unwrap();

        assert_eq!(stats.totals.fire_count, 1);
        assert_eq!(stats.totals.predicate_misses, 1);
        assert_eq!(stats.totals.dlq_count, 1);
        assert_eq!(stats.totals.llm_call_count, 1);
        assert_eq!(stats.providers[0].provider, "openai");
        assert_eq!(stats.triggers[0].trigger_id, "triage");
        assert_eq!(stats.triggers[0].input_tokens, 110);
    }
}
