use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use time::OffsetDateTime;

use crate::package::{CollectedManifestTrigger, CollectedTriggerHandler, TriggerKind};

use super::common::{
    read_topic, trigger_list, ConnectorActivationSnapshot, LoadedOrchestratorContext,
    PersistedStateSnapshot, PersistedTriggerStateSnapshot, TRIGGERS_LIFECYCLE_TOPIC,
    TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_INBOX_LEGACY_TOPIC, TRIGGER_OUTBOX_TOPIC,
};

const GLOBAL_FLOW_KEY: &str = "_global";

#[derive(Clone, Debug, Serialize)]
pub(crate) struct OrchestratorInspectData {
    pub triggers: Vec<TriggerInspectRecord>,
    pub budget: OrchestratorBudgetInspect,
    pub connectors: Vec<String>,
    pub activations: Vec<ConnectorActivationSnapshot>,
    pub snapshot: Option<PersistedStateSnapshot>,
    pub recent_dispatches: Vec<RecentDispatchRecord>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct OrchestratorBudgetInspect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily_limit_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hourly_limit_usd: Option<f64>,
    pub used_today_usd: f64,
    pub used_hour_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_today_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_hour_usd: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct TriggerInspectRecord {
    pub id: String,
    pub provider: String,
    pub kind: String,
    pub handler: String,
    pub version: Option<u32>,
    pub state: Option<String>,
    pub metrics: TriggerInspectMetrics,
    pub flow_control: TriggerFlowControlInspect,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct TriggerInspectMetrics {
    pub received: u64,
    pub dispatched: u64,
    pub failed: u64,
    pub in_flight: u64,
    pub autonomous_decisions_total: u64,
    pub autonomous_decisions_hour: u64,
    pub autonomous_decisions_today: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct TriggerFlowControlInspect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<QueueFlowControlInspect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throttle: Option<ThrottleInspect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitInspect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debounce: Option<DebounceInspect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub singleton: Option<QueueFlowControlInspect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchInspect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<PriorityInspect>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_budget: Option<CostBudgetInspect>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct QueueFlowControlInspect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_expr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<u32>,
    pub queue_depth_by_key: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct ThrottleInspect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_expr: Option<String>,
    pub window_sec: u64,
    pub limit: u32,
    pub queue_depth_by_key: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct RateLimitInspect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_expr: Option<String>,
    pub window_sec: u64,
    pub limit: u32,
    pub util_by_key: BTreeMap<String, RateLimitUtilInspect>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct RateLimitUtilInspect {
    pub window_sec: u64,
    pub count: u64,
    pub limit: u32,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct DebounceInspect {
    pub key_expr: String,
    pub window_sec: u64,
    pub queue_depth_by_key: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct BatchInspect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_expr: Option<String>,
    pub size: u32,
    pub timeout_sec: u64,
    pub queue_depth_by_key: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct PriorityInspect {
    pub key_expr: String,
    pub order: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub(crate) struct CostBudgetInspect {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily_limit_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hourly_limit_usd: Option<f64>,
    pub limit_usd: f64,
    pub used_usd: f64,
    pub used_hour_usd: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_today_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_hour_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_autonomous_decisions_per_hour: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_autonomous_decisions_per_day: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RecentDispatchRecord {
    pub kind: String,
    pub status: String,
    pub occurred_at_ms: i64,
    pub trigger_id: Option<String>,
    pub event_id: Option<String>,
    pub attempt: Option<u32>,
    pub replay_of_event_id: Option<String>,
    pub handler_kind: Option<String>,
    pub target_uri: Option<String>,
    pub error: Option<String>,
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default)]
struct SkipDisposition {
    stage: Option<String>,
    flow_control: Option<String>,
}

pub(crate) async fn collect_orchestrator_inspect_data(
    ctx: &mut LoadedOrchestratorContext,
) -> Result<OrchestratorInspectData, String> {
    let runtime_bindings = trigger_list(ctx).await?;
    let snapshot = ctx.snapshot.clone();
    let snapshot_by_id = snapshot_trigger_map(snapshot.as_ref());
    let runtime_by_id = runtime_trigger_map(&runtime_bindings);
    let current_binding_keys =
        current_binding_keys(&ctx.collected_triggers, &snapshot_by_id, &runtime_by_id);

    let envelopes = read_topic(&ctx.event_log, TRIGGER_INBOX_ENVELOPES_TOPIC).await?;
    let legacy_inbox = read_topic(&ctx.event_log, TRIGGER_INBOX_LEGACY_TOPIC).await?;
    let outbox = read_topic(&ctx.event_log, TRIGGER_OUTBOX_TOPIC).await?;
    let lifecycle = read_topic(&ctx.event_log, TRIGGERS_LIFECYCLE_TOPIC).await?;

    let recent_dispatches = recent_dispatch_records(&outbox, 20);
    let terminal = terminal_dispatches(&outbox);
    let skipped = skipped_dispatches(&outbox);

    let mut pending_by_binding_key = BTreeMap::<String, Vec<harn_vm::TriggerEvent>>::new();
    let mut ingested_by_binding_key = BTreeMap::<String, Vec<harn_vm::TriggerEvent>>::new();

    for (_, record) in envelopes.into_iter().chain(legacy_inbox) {
        if record.kind != "event_ingested" {
            continue;
        }
        let envelope: harn_vm::triggers::dispatcher::InboxEnvelope =
            serde_json::from_value(record.payload)
                .map_err(|error| format!("failed to decode trigger inbox envelope: {error}"))?;
        let binding_keys =
            binding_keys_for_envelope(&envelope, &ctx.collected_triggers, &current_binding_keys);
        for binding_key in binding_keys {
            ingested_by_binding_key
                .entry(binding_key.clone())
                .or_default()
                .push(envelope.event.clone());
            if !terminal.contains(&(binding_key.clone(), envelope.event.id.0.clone())) {
                pending_by_binding_key
                    .entry(binding_key)
                    .or_default()
                    .push(envelope.event.clone());
            }
        }
    }

    let today_start = utc_day_start();
    let hour_start = utc_hour_start();
    let mut cost_by_binding_key = BTreeMap::<String, f64>::new();
    let mut hourly_cost_by_binding_key = BTreeMap::<String, f64>::new();
    for (_, event) in lifecycle {
        if event.kind != "predicate.evaluated" {
            continue;
        }
        let Some(binding_key) = event.headers.get("binding_key").cloned() else {
            continue;
        };
        let cost = event
            .payload
            .get("cost_usd")
            .and_then(|value| value.as_f64())
            .unwrap_or_default();
        if cost > 0.0 {
            if event.occurred_at_ms >= today_start.unix_timestamp() * 1000 {
                *cost_by_binding_key.entry(binding_key.clone()).or_default() += cost;
            }
            if event.occurred_at_ms >= hour_start.unix_timestamp() * 1000 {
                *hourly_cost_by_binding_key.entry(binding_key).or_default() += cost;
            }
        }
    }

    let mut triggers = Vec::new();
    for trigger in ctx.collected_triggers.clone() {
        let snapshot_state = snapshot_by_id.get(&trigger.config.id);
        let runtime_state = runtime_by_id.get(&trigger.config.id);
        let version = snapshot_state
            .and_then(|state| state.version)
            .or_else(|| runtime_state.map(|binding| binding.version));
        let binding_key = version.map(|version| format!("{}@v{}", trigger.config.id, version));
        let pending_events = binding_key
            .as_ref()
            .and_then(|binding_key| pending_by_binding_key.get(binding_key))
            .cloned()
            .unwrap_or_default();
        let ingested_events = binding_key
            .as_ref()
            .and_then(|binding_key| ingested_by_binding_key.get(binding_key))
            .cloned()
            .unwrap_or_default();
        let trigger_metrics = snapshot_state
            .map(trigger_metrics_from_snapshot)
            .or_else(|| runtime_state.map(trigger_metrics_from_runtime))
            .unwrap_or_default();

        let flow_control = build_flow_control_inspect(
            ctx,
            &trigger,
            binding_key.as_deref(),
            &pending_events,
            &ingested_events,
            &skipped,
            &cost_by_binding_key,
            &hourly_cost_by_binding_key,
        )
        .await?;

        triggers.push(TriggerInspectRecord {
            id: trigger.config.id.clone(),
            provider: trigger.config.provider.as_str().to_string(),
            kind: trigger_kind_name(trigger.config.kind).to_string(),
            handler: handler_kind(&trigger.handler).to_string(),
            version,
            state: snapshot_state
                .and_then(|state| state.state.clone())
                .or_else(|| runtime_state.map(|binding| binding.state.as_str().to_string())),
            metrics: trigger_metrics,
            flow_control,
        });
    }

    Ok(OrchestratorInspectData {
        triggers,
        budget: orchestrator_budget_inspect(),
        connectors: snapshot
            .as_ref()
            .map(|state| state.connectors.clone())
            .unwrap_or_default(),
        activations: snapshot
            .as_ref()
            .map(|state| state.activations.clone())
            .unwrap_or_default(),
        snapshot,
        recent_dispatches,
    })
}

pub(crate) fn recent_dispatch_records(
    dispatches: &[(u64, harn_vm::event_log::LogEvent)],
    limit: usize,
) -> Vec<RecentDispatchRecord> {
    let mut recent: Vec<_> = dispatches
        .iter()
        .filter_map(|(_, event)| {
            if !matches!(
                event.kind.as_str(),
                "dispatch_succeeded" | "dispatch_failed" | "dispatch_skipped"
            ) {
                return None;
            }

            let payload = event.payload.as_object();
            Some(RecentDispatchRecord {
                status: event.kind.trim_start_matches("dispatch_").to_string(),
                kind: event.kind.clone(),
                occurred_at_ms: event.occurred_at_ms,
                trigger_id: event.headers.get("trigger_id").cloned(),
                event_id: event.headers.get("event_id").cloned(),
                attempt: event
                    .headers
                    .get("attempt")
                    .and_then(|attempt| attempt.parse::<u32>().ok()),
                replay_of_event_id: event.headers.get("replay_of_event_id").cloned(),
                handler_kind: payload.and_then(|payload| {
                    payload
                        .get("handler_kind")
                        .and_then(|value| value.as_str())
                        .map(ToOwned::to_owned)
                }),
                target_uri: payload.and_then(|payload| {
                    payload
                        .get("target_uri")
                        .and_then(|value| value.as_str())
                        .map(ToOwned::to_owned)
                }),
                error: payload.and_then(|payload| {
                    payload
                        .get("error")
                        .and_then(|value| value.as_str())
                        .map(ToOwned::to_owned)
                }),
                result: payload.and_then(|payload| payload.get("result").cloned()),
                skip_stage: payload.and_then(|payload| {
                    payload
                        .get("skip_stage")
                        .and_then(|value| value.as_str())
                        .map(ToOwned::to_owned)
                }),
                detail: payload.and_then(|payload| payload.get("detail").cloned()),
            })
        })
        .collect();

    recent.sort_by_key(|dispatch| dispatch.occurred_at_ms);
    if recent.len() > limit {
        recent.drain(0..recent.len() - limit);
    }
    recent
}

async fn build_flow_control_inspect(
    ctx: &mut LoadedOrchestratorContext,
    trigger: &CollectedManifestTrigger,
    binding_key: Option<&str>,
    pending_events: &[harn_vm::TriggerEvent],
    ingested_events: &[harn_vm::TriggerEvent],
    skipped: &BTreeMap<(String, String), SkipDisposition>,
    cost_by_binding_key: &BTreeMap<String, f64>,
    hourly_cost_by_binding_key: &BTreeMap<String, f64>,
) -> Result<TriggerFlowControlInspect, String> {
    let flow = &trigger.flow_control;
    let mut inspect = TriggerFlowControlInspect::default();

    if let Some(concurrency) = &flow.concurrency {
        inspect.concurrency = Some(QueueFlowControlInspect {
            key_expr: concurrency.key.as_ref().map(|expr| expr.raw.clone()),
            max: Some(concurrency.max),
            queue_depth_by_key: queue_depth_by_key(ctx, concurrency.key.as_ref(), pending_events)
                .await?,
        });
    }
    if let Some(throttle) = &flow.throttle {
        inspect.throttle = Some(ThrottleInspect {
            key_expr: throttle.key.as_ref().map(|expr| expr.raw.clone()),
            window_sec: throttle.period.as_secs(),
            limit: throttle.max,
            queue_depth_by_key: queue_depth_by_key(ctx, throttle.key.as_ref(), pending_events)
                .await?,
        });
    }
    if let Some(rate_limit) = &flow.rate_limit {
        inspect.rate_limit = Some(RateLimitInspect {
            key_expr: rate_limit.key.as_ref().map(|expr| expr.raw.clone()),
            window_sec: rate_limit.period.as_secs(),
            limit: rate_limit.max,
            util_by_key: rate_limit_util_by_key(
                ctx,
                binding_key,
                rate_limit.key.as_ref(),
                ingested_events,
                skipped,
                rate_limit.period,
                rate_limit.max,
            )
            .await?,
        });
    }
    if let Some(debounce) = &flow.debounce {
        inspect.debounce = Some(DebounceInspect {
            key_expr: debounce.key.raw.clone(),
            window_sec: debounce.period.as_secs(),
            queue_depth_by_key: queue_depth_by_key(ctx, Some(&debounce.key), pending_events)
                .await?,
        });
    }
    if let Some(singleton) = &flow.singleton {
        inspect.singleton = Some(QueueFlowControlInspect {
            key_expr: singleton.key.as_ref().map(|expr| expr.raw.clone()),
            max: None,
            queue_depth_by_key: queue_depth_by_key(ctx, singleton.key.as_ref(), pending_events)
                .await?,
        });
    }
    if let Some(batch) = &flow.batch {
        inspect.batch = Some(BatchInspect {
            key_expr: batch.key.as_ref().map(|expr| expr.raw.clone()),
            size: batch.size,
            timeout_sec: batch.timeout.as_secs(),
            queue_depth_by_key: queue_depth_by_key(ctx, batch.key.as_ref(), pending_events).await?,
        });
    }
    if let Some(priority) = &flow.priority {
        inspect.priority = Some(PriorityInspect {
            key_expr: priority.key.raw.clone(),
            order: priority.order.clone(),
        });
    }
    if let Some(binding_key) = binding_key.filter(|_| {
        trigger.config.budget.daily_cost_usd.is_some()
            || trigger.config.budget.hourly_cost_usd.is_some()
            || trigger
                .config
                .budget
                .max_autonomous_decisions_per_hour
                .is_some()
            || trigger
                .config
                .budget
                .max_autonomous_decisions_per_day
                .is_some()
    }) {
        let used_today = cost_by_binding_key
            .get(binding_key)
            .copied()
            .unwrap_or_default();
        let used_hour = hourly_cost_by_binding_key
            .get(binding_key)
            .copied()
            .unwrap_or_default();
        inspect.cost_budget = Some(CostBudgetInspect {
            daily_limit_usd: trigger.config.budget.daily_cost_usd,
            hourly_limit_usd: trigger.config.budget.hourly_cost_usd,
            limit_usd: trigger.config.budget.daily_cost_usd.unwrap_or_default(),
            used_usd: used_today,
            used_hour_usd: used_hour,
            remaining_today_usd: trigger
                .config
                .budget
                .daily_cost_usd
                .map(|limit| (limit - used_today).max(0.0)),
            remaining_hour_usd: trigger
                .config
                .budget
                .hourly_cost_usd
                .map(|limit| (limit - used_hour).max(0.0)),
            max_autonomous_decisions_per_hour: trigger
                .config
                .budget
                .max_autonomous_decisions_per_hour,
            max_autonomous_decisions_per_day: trigger
                .config
                .budget
                .max_autonomous_decisions_per_day,
        });
    }

    Ok(inspect)
}

async fn queue_depth_by_key(
    ctx: &mut LoadedOrchestratorContext,
    expr: Option<&harn_vm::TriggerExpressionSpec>,
    events: &[harn_vm::TriggerEvent],
) -> Result<BTreeMap<String, u64>, String> {
    let mut counts = BTreeMap::new();
    for event in events {
        let key = evaluate_flow_key(&mut ctx.vm, expr, event).await?;
        *counts.entry(key).or_insert(0) += 1;
    }
    Ok(counts)
}

async fn rate_limit_util_by_key(
    ctx: &mut LoadedOrchestratorContext,
    binding_key: Option<&str>,
    expr: Option<&harn_vm::TriggerExpressionSpec>,
    events: &[harn_vm::TriggerEvent],
    skipped: &BTreeMap<(String, String), SkipDisposition>,
    period: std::time::Duration,
    limit: u32,
) -> Result<BTreeMap<String, RateLimitUtilInspect>, String> {
    let mut util = BTreeMap::new();
    let Some(window_start) =
        OffsetDateTime::now_utc().checked_sub(period.try_into().unwrap_or_default())
    else {
        return Ok(util);
    };
    for event in events {
        if event.received_at < window_start {
            continue;
        }
        if binding_key
            .and_then(|binding_key| skipped.get(&(binding_key.to_string(), event.id.0.clone())))
            .is_some_and(skip_consumed_before_rate_limit)
        {
            continue;
        }
        let key = evaluate_flow_key(&mut ctx.vm, expr, event).await?;
        let entry = util.entry(key).or_insert(RateLimitUtilInspect {
            window_sec: period.as_secs(),
            count: 0,
            limit,
        });
        entry.count += 1;
    }
    Ok(util)
}

async fn evaluate_flow_key(
    vm: &mut harn_vm::Vm,
    expr: Option<&harn_vm::TriggerExpressionSpec>,
    event: &harn_vm::TriggerEvent,
) -> Result<String, String> {
    let Some(expr) = expr else {
        return Ok(GLOBAL_FLOW_KEY.to_string());
    };
    let arg = harn_vm::json_to_vm_value(
        &serde_json::to_value(event)
            .map_err(|error| format!("failed to encode trigger event: {error}"))?,
    );
    let value = vm
        .call_closure_pub(&expr.closure, &[arg])
        .await
        .map_err(|error| {
            format!(
                "failed to evaluate flow-control expression '{}': {error}",
                expr.raw
            )
        })?;
    Ok(json_value_to_gate(&harn_vm::llm::vm_value_to_json(&value)))
}

fn current_binding_keys(
    triggers: &[CollectedManifestTrigger],
    snapshot_by_id: &BTreeMap<String, PersistedTriggerStateSnapshot>,
    runtime_by_id: &BTreeMap<String, harn_vm::TriggerBindingSnapshot>,
) -> BTreeMap<String, String> {
    triggers
        .iter()
        .filter_map(|trigger| {
            let version = snapshot_by_id
                .get(&trigger.config.id)
                .and_then(|state| state.version)
                .or_else(|| {
                    runtime_by_id
                        .get(&trigger.config.id)
                        .map(|binding| binding.version)
                })?;
            Some((
                trigger.config.id.clone(),
                format!("{}@v{}", trigger.config.id, version),
            ))
        })
        .collect()
}

fn binding_keys_for_envelope(
    envelope: &harn_vm::triggers::dispatcher::InboxEnvelope,
    triggers: &[CollectedManifestTrigger],
    current_binding_keys: &BTreeMap<String, String>,
) -> Vec<String> {
    if let Some(trigger_id) = envelope.trigger_id.as_ref() {
        if let Some(version) = envelope.binding_version {
            return vec![format!("{trigger_id}@v{version}")];
        }
        return current_binding_keys
            .get(trigger_id)
            .cloned()
            .into_iter()
            .collect();
    }

    if let harn_vm::ProviderPayload::Known(harn_vm::triggers::event::KnownProviderPayload::Cron(
        payload,
    )) = &envelope.event.provider_payload
    {
        if let Some(cron_id) = payload.cron_id.as_ref() {
            return current_binding_keys
                .get(cron_id)
                .cloned()
                .into_iter()
                .collect();
        }
    }

    triggers
        .iter()
        .filter(|trigger| {
            trigger.config.provider == envelope.event.provider
                && trigger
                    .config
                    .match_
                    .events
                    .iter()
                    .any(|event| event == &envelope.event.kind)
        })
        .filter_map(|trigger| current_binding_keys.get(&trigger.config.id).cloned())
        .collect()
}

fn terminal_dispatches(
    outbox: &[(u64, harn_vm::event_log::LogEvent)],
) -> BTreeSet<(String, String)> {
    outbox
        .iter()
        .filter(|(_, event)| {
            matches!(
                event.kind.as_str(),
                "dispatch_succeeded" | "dispatch_failed" | "dispatch_skipped"
            )
        })
        .filter_map(|(_, event)| {
            Some((
                event.headers.get("binding_key")?.clone(),
                event.headers.get("event_id")?.clone(),
            ))
        })
        .collect()
}

fn skipped_dispatches(
    outbox: &[(u64, harn_vm::event_log::LogEvent)],
) -> BTreeMap<(String, String), SkipDisposition> {
    outbox
        .iter()
        .filter(|(_, event)| event.kind == "dispatch_skipped")
        .filter_map(|(_, event)| {
            let binding_key = event.headers.get("binding_key")?.clone();
            let event_id = event.headers.get("event_id")?.clone();
            let disposition = SkipDisposition {
                stage: event
                    .payload
                    .get("skip_stage")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
                flow_control: event
                    .payload
                    .get("detail")
                    .and_then(|detail| detail.get("flow_control"))
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
            };
            Some(((binding_key, event_id), disposition))
        })
        .collect()
}

fn skip_consumed_before_rate_limit(skip: &SkipDisposition) -> bool {
    match skip.stage.as_deref() {
        Some("predicate") => true,
        Some("flow_control") => matches!(
            skip.flow_control.as_deref(),
            Some("batch_merged") | Some("debounced") | Some("rate_limited")
        ),
        _ => false,
    }
}

fn snapshot_trigger_map(
    snapshot: Option<&PersistedStateSnapshot>,
) -> BTreeMap<String, PersistedTriggerStateSnapshot> {
    snapshot
        .map(|snapshot| {
            snapshot
                .triggers
                .iter()
                .cloned()
                .map(|trigger| (trigger.id.clone(), trigger))
                .collect()
        })
        .unwrap_or_default()
}

fn runtime_trigger_map(
    bindings: &[harn_vm::TriggerBindingSnapshot],
) -> BTreeMap<String, harn_vm::TriggerBindingSnapshot> {
    bindings
        .iter()
        .filter(|binding| binding.source == harn_vm::TriggerBindingSource::Manifest)
        .cloned()
        .map(|binding| (binding.id.clone(), binding))
        .collect()
}

fn trigger_metrics_from_snapshot(
    snapshot: &PersistedTriggerStateSnapshot,
) -> TriggerInspectMetrics {
    TriggerInspectMetrics {
        received: snapshot.received,
        dispatched: snapshot.dispatched,
        failed: snapshot.failed,
        in_flight: snapshot.in_flight,
        autonomous_decisions_total: 0,
        autonomous_decisions_hour: 0,
        autonomous_decisions_today: 0,
    }
}

fn trigger_metrics_from_runtime(
    binding: &harn_vm::TriggerBindingSnapshot,
) -> TriggerInspectMetrics {
    TriggerInspectMetrics {
        received: binding.metrics.received,
        dispatched: binding.metrics.dispatched,
        failed: binding.metrics.failed,
        in_flight: binding.metrics.in_flight,
        autonomous_decisions_total: binding.metrics.autonomous_decisions_total,
        autonomous_decisions_hour: binding.metrics.autonomous_decisions_hour,
        autonomous_decisions_today: binding.metrics.autonomous_decisions_today,
    }
}

fn handler_kind(handler: &CollectedTriggerHandler) -> &'static str {
    match handler {
        CollectedTriggerHandler::Local { .. } => "local",
        CollectedTriggerHandler::A2a { .. } => "a2a",
        CollectedTriggerHandler::Worker { .. } => "worker",
    }
}

fn trigger_kind_name(kind: TriggerKind) -> &'static str {
    match kind {
        TriggerKind::Webhook => "webhook",
        TriggerKind::Cron => "cron",
        TriggerKind::Poll => "poll",
        TriggerKind::Stream => "stream",
        TriggerKind::Predicate => "predicate",
        TriggerKind::A2aPush => "a2a-push",
    }
}

fn json_value_to_gate(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Number(value) => value.to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "unserializable".to_string()),
    }
}

fn utc_day_start() -> OffsetDateTime {
    let now = OffsetDateTime::now_utc();
    now.replace_hour(0)
        .and_then(|value| value.replace_minute(0))
        .and_then(|value| value.replace_second(0))
        .and_then(|value| value.replace_millisecond(0))
        .and_then(|value| value.replace_microsecond(0))
        .and_then(|value| value.replace_nanosecond(0))
        .unwrap_or(now)
}

fn utc_hour_start() -> OffsetDateTime {
    let now = OffsetDateTime::now_utc();
    now.replace_minute(0)
        .and_then(|value| value.replace_second(0))
        .and_then(|value| value.replace_millisecond(0))
        .and_then(|value| value.replace_microsecond(0))
        .and_then(|value| value.replace_nanosecond(0))
        .unwrap_or(now)
}

fn orchestrator_budget_inspect() -> OrchestratorBudgetInspect {
    let snapshot = harn_vm::snapshot_orchestrator_budget();
    let used_today = harn_vm::micros_to_usd(snapshot.cost_today_usd_micros);
    let used_hour = harn_vm::micros_to_usd(snapshot.cost_hour_usd_micros);
    OrchestratorBudgetInspect {
        daily_limit_usd: snapshot.daily_cost_usd,
        hourly_limit_usd: snapshot.hourly_cost_usd,
        used_today_usd: used_today,
        used_hour_usd: used_hour,
        remaining_today_usd: snapshot
            .daily_cost_usd
            .map(|limit| (limit - used_today).max(0.0)),
        remaining_hour_usd: snapshot
            .hourly_cost_usd
            .map(|limit| (limit - used_hour).max(0.0)),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use harn_vm::event_log::{EventLog, LogEvent, Topic};
    use tempfile::TempDir;

    use crate::cli::OrchestratorLocalArgs;
    use crate::tests::common::harn_state_lock::lock_harn_state;

    use super::super::common::load_local_runtime;
    use super::*;

    fn write_file(dir: &Path, relative: &str, contents: &str) {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn write_fixture(temp: &TempDir) {
        write_file(
            temp.path(),
            "harn.toml",
            r#"
[package]
name = "inspect-fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "cron-flow"
kind = "cron"
provider = "cron"
schedule = "* * * * *"
match = { events = ["cron.tick"] }
handler = "handlers::on_ok"
rate_limit = { key = "event.headers.tenant", period = "1h", max = 1 }
singleton = { key = "event.headers.tenant" }
budget = { daily_cost_usd = 1.0 }
"#,
        );
        write_file(
            temp.path(),
            "lib.harn",
            r#"
import "std/triggers"

pub fn on_ok(event: TriggerEvent) -> dict {
  return {tenant: event.headers.tenant}
}
"#,
        );
    }

    fn trigger_event(event_id: &str, tenant: &str) -> harn_vm::TriggerEvent {
        harn_vm::TriggerEvent {
            id: harn_vm::TriggerEventId(event_id.to_string()),
            provider: harn_vm::ProviderId::new("cron"),
            kind: "cron.tick".to_string(),
            received_at: OffsetDateTime::now_utc(),
            occurred_at: None,
            dedupe_key: event_id.to_string(),
            trace_id: harn_vm::TraceId::new(),
            tenant_id: None,
            headers: BTreeMap::from([(String::from("tenant"), tenant.to_string())]),
            raw_body: None,
            batch: None,
            provider_payload: harn_vm::ProviderPayload::Known(
                harn_vm::triggers::event::KnownProviderPayload::Cron(
                    harn_vm::triggers::CronEventPayload {
                        cron_id: Some("cron-flow".to_string()),
                        schedule: Some("* * * * *".to_string()),
                        tick_at: OffsetDateTime::now_utc(),
                        raw: serde_json::json!({}),
                    },
                ),
            ),
            signature_status: harn_vm::SignatureStatus::Verified,
            dedupe_claimed: false,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::await_holding_lock)]
    async fn collect_orchestrator_inspect_data_reports_flow_control_state() {
        let _guard = lock_harn_state();
        let temp = TempDir::new().unwrap();
        write_fixture(&temp);

        let mut ctx = load_local_runtime(&OrchestratorLocalArgs {
            config: temp.path().join("harn.toml"),
            state_dir: temp.path().join("state"),
        })
        .await
        .expect("load local runtime");

        let inbox_topic = Topic::new(TRIGGER_INBOX_ENVELOPES_TOPIC).unwrap();
        let outbox_topic = Topic::new(TRIGGER_OUTBOX_TOPIC).unwrap();
        let lifecycle_topic = Topic::new(TRIGGERS_LIFECYCLE_TOPIC).unwrap();

        let event_one = trigger_event("evt-1", "acme");
        let event_two = trigger_event("evt-2", "acme");

        let envelope_one = harn_vm::triggers::dispatcher::InboxEnvelope {
            trigger_id: Some("cron-flow".to_string()),
            binding_version: Some(1),
            event: event_one.clone(),
        };
        let envelope_two = harn_vm::triggers::dispatcher::InboxEnvelope {
            trigger_id: Some("cron-flow".to_string()),
            binding_version: Some(1),
            event: event_two.clone(),
        };

        ctx.event_log
            .append(
                &inbox_topic,
                LogEvent::new(
                    "event_ingested",
                    serde_json::to_value(&envelope_one).unwrap(),
                ),
            )
            .await
            .unwrap();
        ctx.event_log
            .append(
                &inbox_topic,
                LogEvent::new(
                    "event_ingested",
                    serde_json::to_value(&envelope_two).unwrap(),
                ),
            )
            .await
            .unwrap();
        ctx.event_log
            .append(
                &outbox_topic,
                LogEvent::new(
                    "dispatch_skipped",
                    serde_json::json!({
                        "event_id": "evt-2",
                        "trigger_id": "cron-flow",
                        "binding_key": "cron-flow@v1",
                        "handler_kind": "local",
                        "target_uri": "local://cron-flow",
                        "skip_stage": "flow_control",
                        "detail": {
                            "flow_control": "rate_limited",
                        },
                    }),
                )
                .with_headers(BTreeMap::from([
                    (String::from("event_id"), String::from("evt-2")),
                    (String::from("trigger_id"), String::from("cron-flow")),
                    (String::from("binding_key"), String::from("cron-flow@v1")),
                ])),
            )
            .await
            .unwrap();
        ctx.event_log
            .append(
                &lifecycle_topic,
                LogEvent::new(
                    "predicate.evaluated",
                    serde_json::json!({
                        "cost_usd": 0.42,
                    }),
                )
                .with_headers(BTreeMap::from([(
                    String::from("binding_key"),
                    String::from("cron-flow@v1"),
                )])),
            )
            .await
            .unwrap();

        let inspect = collect_orchestrator_inspect_data(&mut ctx)
            .await
            .expect("collect inspect data");
        let trigger = inspect
            .triggers
            .iter()
            .find(|trigger| trigger.id == "cron-flow")
            .expect("cron-flow trigger in inspect payload");

        assert_eq!(
            trigger
                .flow_control
                .singleton
                .as_ref()
                .and_then(|singleton| singleton.queue_depth_by_key.get("acme"))
                .copied(),
            Some(1)
        );
        assert_eq!(
            trigger
                .flow_control
                .rate_limit
                .as_ref()
                .and_then(|rate_limit| rate_limit.util_by_key.get("acme"))
                .map(|util| util.count),
            Some(1)
        );
        assert_eq!(
            trigger
                .flow_control
                .cost_budget
                .as_ref()
                .map(|budget| budget.used_usd),
            Some(0.42)
        );
    }
}
