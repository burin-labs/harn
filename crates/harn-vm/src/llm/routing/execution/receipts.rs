use std::collections::BTreeMap;

use crate::value::VmValue;

use super::super::RoutingPolicyConfig;

/// What `execute_with_routing` returns about each attempt so the
/// caller can re-emit it as a `routing_decision` block on the user-
/// facing result envelope.
#[derive(Clone, Debug)]
pub(crate) struct RoutingTrace {
    pub label: String,
    pub attempts: Vec<RoutingAttempt>,
    pub selected: Option<usize>,
    pub session_cost_usd: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct RoutingAttempt {
    pub index: usize,
    pub provider: String,
    pub model: String,
    pub label: String,
    pub status: AttemptStatus,
    pub duration_ms: u64,
    pub cost_usd: Option<f64>,
    /// Token counts attributed to this attempt's provider call. Mirrors
    /// the winning `LlmResult.input_tokens` / `output_tokens` so
    /// downstream graders can re-compute spend against alternate
    /// pricing tables (e.g. OpenRouter, which isn't in the catalog).
    /// `None` when the attempt was skipped, race-lost, or budget-aborted
    /// before the provider returned a payload.
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub error: Option<RoutingErrorSnapshot>,
    /// Per-verifier signals emitted after this attempt's response.
    /// Empty when the policy has no `escalate_on` chain or the
    /// attempt failed before verifiers ran.
    pub verifier_signals: Vec<VerifierSignalRecord>,
    /// What the verifier chain told the router to do with this
    /// candidate. `Accept` means the answer was returned;
    /// `Refine`/`Escalate` mean the router moved on (within the same
    /// link or to the next link respectively).
    pub verifier_outcome: Option<VerifierOutcome>,
}

/// One signal entry in `RoutingAttempt.verifier_signals`. Kept simple
/// so it survives serialization through `VmValue` dicts without losing
/// the verifier's name and the human-readable reason.
#[derive(Clone, Debug)]
pub(crate) struct VerifierSignalRecord {
    pub name: String,
    pub kind: String,
    pub signal: String,
    pub reason: Option<String>,
}

/// Aggregated verdict across the verifier chain for one attempt. Drives
/// receipt rendering ("escalated because verifier=refine on attempt 1,
/// escalate on attempt 2").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VerifierOutcome {
    Accept,
    Refine,
    Escalate,
}

impl VerifierOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Refine => "refine",
            Self::Escalate => "escalate",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum AttemptStatus {
    Succeeded,
    Failed,
    Skipped,
    /// Cancelled because a concurrent racer won.
    RaceLost,
}

impl AttemptStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::RaceLost => "race_lost",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RoutingErrorSnapshot {
    pub category: String,
    pub code: Option<String>,
    pub reason: Option<String>,
    pub attempt_count: Option<usize>,
    pub message: String,
    pub status: Option<u16>,
}

/// Match an error against the policy's failover rules. Returns true
/// when the error is eligible to advance the chain.

pub(super) fn emit_routing_event(
    dispatch: &str,
    event: &str,
    metadata: serde_json::Map<String, serde_json::Value>,
) {
    let category = format!("{dispatch}.{event}");
    let mut meta: BTreeMap<String, serde_json::Value> = metadata.into_iter().collect();
    meta.entry("event".to_string())
        .or_insert_with(|| serde_json::Value::String(event.to_string()));
    crate::events::log_info_meta(&category, "", meta);
}

/// Encode the routing trace into the standard `LlmRoutingDecision`
/// shape so the result envelope, transcript provider_payload, and
/// portal all see one consistent schema.
pub(crate) fn trace_to_decision(
    trace: &RoutingTrace,
    policy: &RoutingPolicyConfig,
) -> crate::llm::api::LlmRoutingDecision {
    use crate::llm::api::{LlmRouteAlternative, LlmRoutingDecision};

    let mut alternatives = Vec::with_capacity(trace.attempts.len());
    for (idx, attempt) in trace.attempts.iter().enumerate() {
        let selected = trace.selected == Some(idx);
        let reason = match attempt.status {
            AttemptStatus::Succeeded => match attempt.verifier_outcome {
                Some(VerifierOutcome::Refine) => {
                    if selected {
                        "selected:verifier_refine_fallback".to_string()
                    } else {
                        "verifier:refine".to_string()
                    }
                }
                Some(VerifierOutcome::Escalate) => {
                    if selected {
                        "selected:verifier_escalate_fallback".to_string()
                    } else {
                        "verifier:escalate".to_string()
                    }
                }
                Some(VerifierOutcome::Accept) | None => "selected".to_string(),
            },
            AttemptStatus::Failed => attempt
                .error
                .as_ref()
                .map(|e| format!("failed:{}", e.category))
                .unwrap_or_else(|| "failed".to_string()),
            AttemptStatus::Skipped => "skipped:budget".to_string(),
            AttemptStatus::RaceLost => "race_lost".to_string(),
        };
        let quality_tier = crate::llm_config::model_tier(&attempt.model);
        let pricing = crate::llm::cost::pricing_per_1k_for(&attempt.provider, &attempt.model);
        alternatives.push(LlmRouteAlternative {
            available: true,
            cost_per_1k_in: pricing.map(|p| p.0),
            cost_per_1k_out: pricing.map(|p| p.1),
            latency_p50_ms: crate::llm::cost::latency_p50_ms_for(&attempt.provider),
            provider: attempt.provider.clone(),
            model: attempt.model.clone(),
            quality_tier,
            selected,
            reason,
        });
    }
    let selected_idx = trace.selected.unwrap_or(0);
    let (selected_provider, selected_model) = trace
        .attempts
        .get(selected_idx)
        .map(|a| (a.provider.clone(), a.model.clone()))
        .unwrap_or_else(|| {
            policy
                .chain
                .first()
                .map(|link| (link.provider.clone(), link.model.clone()))
                .unwrap_or_default()
        });
    LlmRoutingDecision {
        policy: format!("routing_policy({})", policy.label),
        requested_quality: None,
        selected_provider,
        selected_model,
        alternatives,
    }
}

/// Convert routing trace attempts into a list of dicts suitable for
/// surfacing on the user-facing result envelope under
/// `routing.attempts`.
pub(crate) fn trace_to_vm_attempts(trace: &RoutingTrace) -> VmValue {
    let items: Vec<VmValue> = trace
        .attempts
        .iter()
        .map(|attempt| {
            let mut dict = BTreeMap::new();
            dict.insert("index".to_string(), VmValue::Int(attempt.index as i64));
            dict.put_str("provider", attempt.provider.clone());
            dict.put_str("model", attempt.model.clone());
            dict.put_str("label", attempt.label.clone());
            dict.put_str("status", attempt.status.as_str());
            dict.insert(
                "duration_ms".to_string(),
                VmValue::Int(attempt.duration_ms as i64),
            );
            if let Some(cost) = attempt.cost_usd {
                dict.insert("cost_usd".to_string(), VmValue::Float(cost));
            }
            if let Some(tokens) = attempt.input_tokens {
                dict.insert("input_tokens".to_string(), VmValue::Int(tokens));
            }
            if let Some(tokens) = attempt.output_tokens {
                dict.insert("output_tokens".to_string(), VmValue::Int(tokens));
            }
            if let Some(error) = &attempt.error {
                let mut err_dict = BTreeMap::new();
                err_dict.put_str("category", error.category.clone());
                err_dict.put_str("message", error.message.clone());
                if let Some(code) = &error.code {
                    err_dict.put_str("code", code.clone());
                }
                if let Some(reason) = &error.reason {
                    err_dict.put_str("reason", reason.clone());
                }
                if let Some(attempt_count) = error.attempt_count {
                    err_dict.insert(
                        "attempt_count".to_string(),
                        VmValue::Int(attempt_count as i64),
                    );
                }
                if let Some(status) = error.status {
                    err_dict.insert("status".to_string(), VmValue::Int(status as i64));
                }
                dict.insert("error".to_string(), VmValue::dict(err_dict));
            }
            if let Some(outcome) = attempt.verifier_outcome {
                dict.put_str("verifier_outcome", outcome.as_str());
            }
            if !attempt.verifier_signals.is_empty() {
                let signals: Vec<VmValue> = attempt
                    .verifier_signals
                    .iter()
                    .map(|signal| {
                        let mut sig_dict = BTreeMap::new();
                        sig_dict.put_str("name", signal.name.clone());
                        sig_dict.put_str("kind", signal.kind.clone());
                        sig_dict.put_str("signal", signal.signal.clone());
                        if let Some(reason) = &signal.reason {
                            sig_dict.put_str("reason", reason.clone());
                        }
                        VmValue::dict(sig_dict)
                    })
                    .collect();
                dict.insert(
                    "verifier_signals".to_string(),
                    VmValue::List(std::sync::Arc::new(signals)),
                );
            }
            VmValue::dict(dict)
        })
        .collect();
    VmValue::List(std::sync::Arc::new(items))
}
