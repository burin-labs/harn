use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::event_log::{EventLog, LogEvent, Topic};
use crate::llm::trigger_predicate::{start_predicate_evaluation, PredicateCacheEntry};
use crate::triggers::registry::{
    binding_budget_would_exceed, expected_predicate_cost_usd_micros, micros_to_usd,
    note_orchestrator_budget_cost, orchestrator_budget_would_exceed, record_predicate_cost_sample,
    reset_binding_budget_windows, usd_to_micros, TriggerBinding, TriggerBudgetExhaustionStrategy,
    TriggerPredicateSpec,
};
use crate::trust_graph::AutonomyTier;
use crate::value::VmValue;

use super::super::TRIGGER_INBOX_LEGACY_TOPIC;
use super::{now_unix_ms, DispatchError, Dispatcher, TriggerEvent};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PredicateCacheRecord {
    trigger_id: String,
    event_id: String,
    entries: Vec<PredicateCacheEntry>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct PredicateEvaluationRecord {
    pub(super) result: bool,
    pub(super) cost_usd: f64,
    pub(super) tokens: u64,
    pub(super) latency_ms: u64,
    pub(super) cached: bool,
    pub(super) reason: Option<String>,
    pub(super) exhaustion_strategy: Option<TriggerBudgetExhaustionStrategy>,
}

impl Dispatcher {
    pub(super) async fn evaluate_predicate(
        &self,
        binding: &TriggerBinding,
        predicate: &TriggerPredicateSpec,
        event: &TriggerEvent,
        replay_of_event_id: Option<&String>,
        autonomy_tier: AutonomyTier,
    ) -> Result<PredicateEvaluationRecord, DispatchError> {
        let event_id = event.id.0.clone();
        let trigger_id = binding.id.as_str().to_string();
        let now_ms = now_unix_ms();
        reset_binding_budget_windows(binding);

        let breaker_open_until = {
            let state = binding
                .predicate_state
                .lock()
                .expect("trigger predicate state poisoned");
            if state
                .breaker_open_until_ms
                .is_some_and(|until_ms| until_ms > now_ms)
            {
                state.breaker_open_until_ms
            } else {
                None
            }
        };

        if breaker_open_until.is_some() {
            let mut metadata = BTreeMap::new();
            metadata.insert("trigger_id".to_string(), serde_json::json!(trigger_id));
            metadata.insert("event_id".to_string(), serde_json::json!(event_id));
            metadata.insert(
                "breaker_open_until_ms".to_string(),
                serde_json::json!(breaker_open_until),
            );
            crate::events::log_warn_meta(
                "trigger.predicate.circuit_breaker",
                "trigger predicate circuit breaker is open; short-circuiting to false",
                metadata,
            );
            let record = PredicateEvaluationRecord {
                result: false,
                reason: Some("circuit_open".to_string()),
                ..Default::default()
            };
            self.append_predicate_evaluated_event(binding, event, &record, replay_of_event_id)
                .await?;
            return Ok(record);
        }

        let expected_cost = expected_predicate_cost_usd_micros(binding);
        if let Some(reason) = binding_budget_would_exceed(binding, expected_cost)
            .or_else(|| orchestrator_budget_would_exceed(expected_cost))
        {
            self.append_budget_exhausted_event(
                binding,
                event,
                reason,
                expected_cost,
                None,
                replay_of_event_id,
            )
            .await?;
            let record = PredicateEvaluationRecord {
                result: binding.on_budget_exhausted == TriggerBudgetExhaustionStrategy::Warn,
                reason: Some(reason.to_string()),
                exhaustion_strategy: Some(binding.on_budget_exhausted),
                ..Default::default()
            };
            self.append_predicate_evaluated_event(binding, event, &record, replay_of_event_id)
                .await?;
            return Ok(record);
        }

        let replay_cache = self
            .read_predicate_cache_record(replay_of_event_id.unwrap_or(&event_id))
            .await?;
        let guard = start_predicate_evaluation(
            binding.when_budget.clone().unwrap_or_default(),
            replay_cache,
        );
        let started = Instant::now();
        let eval = self
            .invoke_vm_callable_with_timeout(
                &predicate.closure,
                &binding.binding_key(),
                event,
                replay_of_event_id,
                binding.id.as_str(),
                &format!("{}.{}", event.provider.as_str(), event.kind),
                autonomy_tier,
                &mut self.cancel_tx.subscribe(),
                binding
                    .when_budget
                    .as_ref()
                    .and_then(|budget| budget.timeout()),
            )
            .await;
        let capture = guard.finish();
        let latency_ms = started.elapsed().as_millis() as u64;
        if replay_of_event_id.is_none() && !capture.entries.is_empty() {
            self.append_predicate_cache_record(binding, event, &capture.entries)
                .await?;
        }

        let mut record = PredicateEvaluationRecord {
            result: false,
            cost_usd: capture.total_cost_usd,
            tokens: capture.total_tokens,
            latency_ms,
            cached: capture.cached,
            reason: None,
            exhaustion_strategy: None,
        };

        let mut count_failure = false;
        let mut opened_breaker = false;

        match eval {
            Ok(value) => match predicate_value_as_bool(value) {
                Ok(result) => {
                    record.result = result;
                }
                Err(reason) => {
                    count_failure = true;
                    record.reason = Some(reason);
                }
            },
            Err(error) => {
                count_failure = true;
                record.reason = Some(error.to_string());
            }
        }

        let cost_usd_micros = usd_to_micros(record.cost_usd);
        if cost_usd_micros > 0 {
            binding
                .metrics
                .cost_total_usd_micros
                .fetch_add(cost_usd_micros, Ordering::Relaxed);
            binding
                .metrics
                .cost_today_usd_micros
                .fetch_add(cost_usd_micros, Ordering::Relaxed);
            binding
                .metrics
                .cost_hour_usd_micros
                .fetch_add(cost_usd_micros, Ordering::Relaxed);
            note_orchestrator_budget_cost(cost_usd_micros);
            record_predicate_cost_sample(binding, cost_usd_micros);
        }

        let timed_out = matches!(
            record.reason.as_deref(),
            Some("predicate evaluation timed out")
        );
        if capture.budget_exceeded || timed_out {
            if binding.on_budget_exhausted != TriggerBudgetExhaustionStrategy::Warn {
                record.result = false;
            } else if timed_out {
                record.result = true;
            }
            record.reason = Some("budget_exceeded".to_string());
            record.exhaustion_strategy = Some(binding.on_budget_exhausted);
            self.append_budget_exhausted_event(
                binding,
                event,
                "budget_exceeded",
                cost_usd_micros,
                Some(record.tokens),
                replay_of_event_id,
            )
            .await?;
            self.append_lifecycle_event(
                "predicate.invocation_budget_exceeded",
                event,
                binding,
                serde_json::json!({
                    "trigger_id": binding.id.as_str(),
                    "event_id": event.id.0,
                    "max_cost_usd": binding.when_budget.as_ref().and_then(|budget| budget.max_cost_usd),
                    "tokens_max": binding.when_budget.as_ref().and_then(|budget| budget.tokens_max),
                    "cost_usd": record.cost_usd,
                    "tokens": record.tokens,
                    "replay_of_event_id": replay_of_event_id,
                }),
                replay_of_event_id,
            )
            .await?;
        }

        if let Some(reason) =
            binding_budget_would_exceed(binding, 0).or_else(|| orchestrator_budget_would_exceed(0))
        {
            if binding.on_budget_exhausted != TriggerBudgetExhaustionStrategy::Warn {
                record.result = false;
            }
            record.reason = Some(reason.to_string());
            record.exhaustion_strategy = Some(binding.on_budget_exhausted);
            self.append_budget_exhausted_event(binding, event, reason, 0, None, replay_of_event_id)
                .await?;
        }

        {
            let mut state = binding
                .predicate_state
                .lock()
                .expect("trigger predicate state poisoned");
            if count_failure {
                state.consecutive_failures = state.consecutive_failures.saturating_add(1);
                if state.consecutive_failures >= 3 {
                    state.breaker_open_until_ms = Some(now_ms.saturating_add(5 * 60 * 1000));
                    opened_breaker = true;
                }
            } else {
                state.consecutive_failures = 0;
                state.breaker_open_until_ms = None;
            }
        }

        if opened_breaker {
            let mut metadata = BTreeMap::new();
            metadata.insert(
                "trigger_id".to_string(),
                serde_json::json!(binding.id.as_str()),
            );
            metadata.insert("event_id".to_string(), serde_json::json!(event.id.0));
            metadata.insert("failure_count".to_string(), serde_json::json!(3));
            metadata.insert("reason".to_string(), serde_json::json!(record.reason));
            crate::events::log_warn_meta(
                "trigger.predicate.circuit_breaker",
                "trigger predicate circuit breaker opened for 5 minutes",
                metadata,
            );
        }

        self.append_predicate_evaluated_event(binding, event, &record, replay_of_event_id)
            .await?;
        Ok(record)
    }

    async fn append_predicate_evaluated_event(
        &self,
        binding: &TriggerBinding,
        event: &TriggerEvent,
        record: &PredicateEvaluationRecord,
        replay_of_event_id: Option<&String>,
    ) -> Result<(), DispatchError> {
        if let Some(metrics) = self.metrics.as_ref() {
            metrics.record_trigger_predicate_evaluation(
                binding.id.as_str(),
                record.result,
                record.cost_usd,
            );
            metrics.set_trigger_budget_cost_today(
                binding.id.as_str(),
                current_predicate_daily_cost(binding),
            );
            if matches!(
                record.reason.as_deref(),
                Some(
                    "budget_exceeded"
                        | "daily_budget_exceeded"
                        | "hourly_budget_exceeded"
                        | "orchestrator_daily_budget_exceeded"
                        | "orchestrator_hourly_budget_exceeded"
                )
            ) {
                metrics.record_trigger_budget_exhausted(
                    binding.id.as_str(),
                    record
                        .exhaustion_strategy
                        .map(TriggerBudgetExhaustionStrategy::as_str)
                        .unwrap_or_else(|| record.reason.as_deref().unwrap_or("predicate")),
                );
            }
        }
        self.append_lifecycle_event(
            "predicate.evaluated",
            event,
            binding,
            serde_json::json!({
                "trigger_id": binding.id.as_str(),
                "event_id": event.id.0,
                "result": record.result,
                "cost_usd": record.cost_usd,
                "tokens": record.tokens,
                "latency_ms": record.latency_ms,
                "cached": record.cached,
                "reason": record.reason,
                "on_budget_exhausted": record.exhaustion_strategy.map(TriggerBudgetExhaustionStrategy::as_str),
                "replay_of_event_id": replay_of_event_id,
            }),
            replay_of_event_id,
        )
        .await
    }

    async fn append_predicate_cache_record(
        &self,
        binding: &TriggerBinding,
        event: &TriggerEvent,
        entries: &[PredicateCacheEntry],
    ) -> Result<(), DispatchError> {
        let topic = Topic::new(TRIGGER_INBOX_LEGACY_TOPIC)
            .expect("static trigger inbox legacy topic name is valid");
        let payload = serde_json::to_value(PredicateCacheRecord {
            trigger_id: binding.id.as_str().to_string(),
            event_id: event.id.0.clone(),
            entries: entries.to_vec(),
        })
        .map_err(|error| DispatchError::Serde(error.to_string()))?;
        self.event_log
            .append(&topic, LogEvent::new("predicate_llm_cache", payload))
            .await
            .map_err(DispatchError::from)
            .map(|_| ())
    }

    async fn read_predicate_cache_record(
        &self,
        event_id: &str,
    ) -> Result<Vec<PredicateCacheEntry>, DispatchError> {
        let topic = Topic::new(TRIGGER_INBOX_LEGACY_TOPIC)
            .expect("static trigger inbox legacy topic name is valid");
        let records = self
            .event_log
            .read_range(&topic, None, usize::MAX)
            .await
            .map_err(DispatchError::from)?;
        Ok(records
            .into_iter()
            .filter(|(_, event)| event.kind == "predicate_llm_cache")
            .filter_map(|(_, event)| {
                serde_json::from_value::<PredicateCacheRecord>(event.payload).ok()
            })
            .filter(|record| record.event_id == event_id)
            .flat_map(|record| record.entries)
            .collect())
    }

    async fn append_budget_exhausted_event(
        &self,
        binding: &TriggerBinding,
        event: &TriggerEvent,
        reason: &str,
        expected_cost_usd_micros: u64,
        tokens: Option<u64>,
        replay_of_event_id: Option<&String>,
    ) -> Result<(), DispatchError> {
        let payload = serde_json::json!({
            "trigger_id": binding.id.as_str(),
            "event_id": event.id.0,
            "reason": reason,
            "strategy": binding.on_budget_exhausted.as_str(),
            "expected_cost_usd": micros_to_usd(expected_cost_usd_micros),
            "cost_usd": micros_to_usd(expected_cost_usd_micros),
            "tokens": tokens,
            "daily_limit_usd": binding.daily_cost_usd,
            "hourly_limit_usd": binding.hourly_cost_usd,
            "cost_today_usd": current_predicate_daily_cost(binding),
            "cost_hour_usd": current_predicate_hourly_cost(binding),
            "replay_of_event_id": replay_of_event_id,
        });
        self.append_lifecycle_event(
            "predicate.budget_exceeded",
            event,
            binding,
            payload.clone(),
            replay_of_event_id,
        )
        .await?;
        let legacy_kind = match reason {
            "daily_budget_exceeded" => Some("predicate.daily_budget_exceeded"),
            "hourly_budget_exceeded" => Some("predicate.hourly_budget_exceeded"),
            "orchestrator_daily_budget_exceeded" => {
                Some("predicate.orchestrator_daily_budget_exceeded")
            }
            "orchestrator_hourly_budget_exceeded" => {
                Some("predicate.orchestrator_hourly_budget_exceeded")
            }
            _ => None,
        };
        if let Some(kind) = legacy_kind {
            self.append_lifecycle_event(kind, event, binding, payload, replay_of_event_id)
                .await?;
        }
        Ok(())
    }
}

pub(super) fn predicate_node_metadata(
    binding: &TriggerBinding,
    predicate: &TriggerPredicateSpec,
    event: &TriggerEvent,
    evaluation: &PredicateEvaluationRecord,
) -> BTreeMap<String, serde_json::Value> {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "trigger_id".to_string(),
        serde_json::json!(binding.id.as_str()),
    );
    metadata.insert("predicate".to_string(), serde_json::json!(predicate.raw));
    metadata.insert("result".to_string(), serde_json::json!(evaluation.result));
    metadata.insert(
        "cost_usd".to_string(),
        serde_json::json!(evaluation.cost_usd),
    );
    metadata.insert("tokens".to_string(), serde_json::json!(evaluation.tokens));
    metadata.insert(
        "latency_ms".to_string(),
        serde_json::json!(evaluation.latency_ms),
    );
    metadata.insert("cached".to_string(), serde_json::json!(evaluation.cached));
    metadata.insert("event_id".to_string(), serde_json::json!(event.id.0));
    if let Some(reason) = evaluation.reason.as_ref() {
        metadata.insert("reason".to_string(), serde_json::json!(reason));
    }
    metadata
}

fn predicate_value_as_bool(value: VmValue) -> Result<bool, String> {
    match value {
        VmValue::Bool(result) => Ok(result),
        VmValue::EnumVariant {
            enum_name,
            variant,
            fields,
        } if enum_name.as_ref() == "Result" && variant.as_ref() == "Ok" => match fields.first() {
            Some(VmValue::Bool(result)) => Ok(*result),
            Some(other) => Err(format!(
                "predicate Result.Ok payload must be bool, got {}",
                other.type_name()
            )),
            None => Err("predicate Result.Ok payload is missing".to_string()),
        },
        VmValue::EnumVariant {
            enum_name,
            variant,
            fields,
        } if enum_name.as_ref() == "Result" && variant.as_ref() == "Err" => Err(fields
            .first()
            .map(VmValue::display)
            .unwrap_or_else(|| "predicate returned Result.Err".to_string())),
        other => Err(format!(
            "predicate must return bool or Result<bool, _>, got {}",
            other.type_name()
        )),
    }
}

fn current_predicate_daily_cost(binding: &TriggerBinding) -> f64 {
    micros_to_usd(
        binding
            .metrics
            .cost_today_usd_micros
            .load(Ordering::Relaxed),
    )
}

fn current_predicate_hourly_cost(binding: &TriggerBinding) -> f64 {
    micros_to_usd(binding.metrics.cost_hour_usd_micros.load(Ordering::Relaxed))
}
