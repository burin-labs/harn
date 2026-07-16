use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::value::{ErrorCategory, VmError, VmValue};

use crate::llm::api::LlmCallOptions;
use crate::llm::cost::{calculate_cost_for_provider, peek_total_cost};
use crate::llm::routing_verifier::{build_refine_nudge, run_verifier, Verifier, VerifierSignal};

use super::{
    auth, runtime_error, BudgetExceedAction, ChainLink, FailoverRules, RoutingPolicyConfig,
    DEFAULT_FAILOVER_STATUSES, DEFAULT_RACE_PRIMARY_TIMEOUT_MS,
};

mod race;
mod receipts;

use race::run_race;
use receipts::emit_routing_event;
pub(crate) use receipts::{
    trace_to_decision, trace_to_vm_attempts, AttemptStatus, RoutingAttempt, RoutingErrorSnapshot,
    RoutingTrace, VerifierOutcome, VerifierSignalRecord,
};

pub(super) fn matches_failover(
    rules: &FailoverRules,
    error: &VmError,
) -> (bool, RoutingErrorSnapshot) {
    let category = crate::value::error_to_category(error);
    let structured = match error {
        VmError::Thrown(VmValue::Dict(fields)) => Some(fields),
        _ => None,
    };
    let message = match error {
        VmError::CategorizedError { message, .. } => message.clone(),
        VmError::Thrown(VmValue::String(s)) => s.to_string(),
        VmError::Thrown(VmValue::Dict(d)) => d
            .get("message")
            .map(|v| v.display())
            .unwrap_or_else(|| error.to_string()),
        _ => error.to_string(),
    };
    let status = extract_status_code(error);
    let snapshot = RoutingErrorSnapshot {
        category: category.as_str().to_string(),
        code: structured.and_then(|fields| vm_string_field(fields, "code")),
        reason: structured.and_then(|fields| vm_string_field(fields, "reason")),
        attempt_count: structured
            .and_then(|fields| fields.get("attempt_count"))
            .and_then(VmValue::as_int)
            .and_then(|value| usize::try_from(value).ok()),
        message,
        status,
    };

    if rules.on_no_dispatch && is_no_dispatch_contract_violation(&snapshot.message) {
        return (true, snapshot);
    }

    if let Some(code) = status {
        if rules.on_status.contains(&code) {
            return (true, snapshot);
        }
    }

    if matches!(category, ErrorCategory::Timeout) && rules.on_timeout_ms.is_some() {
        return (true, snapshot);
    }

    let category_label = category.as_str();
    let kind_match = rules.on_error_kinds.iter().any(|kind| {
        let normalized = kind.trim().to_ascii_lowercase();
        if normalized == category_label {
            return true;
        }
        matches!(
            (normalized.as_str(), category.clone()),
            ("rate_limit", ErrorCategory::RateLimit)
                | ("overloaded", ErrorCategory::Overloaded)
                | ("transient", ErrorCategory::TransientNetwork)
                | ("transient_network", ErrorCategory::TransientNetwork)
                | ("network", ErrorCategory::TransientNetwork)
                | ("timeout", ErrorCategory::Timeout)
                | ("schema_validation", ErrorCategory::SchemaValidation)
                | ("auth", ErrorCategory::Auth)
                | ("provider_error", ErrorCategory::ServerError)
                | ("server_error", ErrorCategory::ServerError)
                | ("provider_5xx", ErrorCategory::ServerError)
                | ("generic", ErrorCategory::Generic)
                | ("budget_exceeded", ErrorCategory::BudgetExceeded)
                | ("circuit_open", ErrorCategory::CircuitOpen)
                | ("egress_blocked", ErrorCategory::EgressBlocked)
                | ("cancelled", ErrorCategory::Cancelled)
                | ("tool_error", ErrorCategory::ToolError)
                | ("tool_rejected", ErrorCategory::ToolRejected)
                | ("not_found", ErrorCategory::NotFound)
        )
    });
    if kind_match {
        return (true, snapshot);
    }

    // Sensible defaults: 429 / 5xx, transient categories, and provider
    // health-circuit errors are always failover-eligible when the script didn't
    // write explicit rules.
    let defaults_active = rules.on_status.is_empty()
        && rules.on_error_kinds.is_empty()
        && rules.on_timeout_ms.is_none();
    if defaults_active {
        let by_status = status
            .map(|code| DEFAULT_FAILOVER_STATUSES.contains(&code))
            .unwrap_or(false);
        let by_category = matches!(
            category,
            ErrorCategory::RateLimit
                | ErrorCategory::Overloaded
                | ErrorCategory::TransientNetwork
                | ErrorCategory::Timeout
                | ErrorCategory::CircuitOpen
                | ErrorCategory::ServerError
        );
        if by_status || by_category {
            return (true, snapshot);
        }
    }

    (false, snapshot)
}

fn vm_string_field(fields: &crate::value::DictMap, key: &str) -> Option<String> {
    match fields.get(key) {
        Some(VmValue::String(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn is_no_dispatch_contract_violation(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("returned billed output")
        && lower.contains("completion_tokens=")
        && lower.contains("no dispatchable tool call or answer")
        && lower.contains("upstream contract violation")
}

fn extract_status_code(error: &VmError) -> Option<u16> {
    let message = error.to_string();
    extract_status_from_text(&message)
}

fn extract_status_from_text(message: &str) -> Option<u16> {
    let lowered = message.to_ascii_lowercase();
    let needles = ["http ", "status_code: ", "status: ", "status "];
    for needle in needles.iter() {
        if let Some(idx) = lowered.find(needle) {
            let tail = &message[idx + needle.len()..];
            if let Some(code) = parse_leading_status(tail) {
                return Some(code);
            }
        }
    }
    parse_leading_status(message)
}

fn parse_leading_status(text: &str) -> Option<u16> {
    let text = text.trim_start();
    let digits: String = text.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits
        .parse::<u16>()
        .ok()
        .filter(|code| (100..=599).contains(code))
}

fn budget_overrun_snapshot(
    cap: f64,
    projected: f64,
    session: f64,
    kind: &str,
) -> RoutingErrorSnapshot {
    RoutingErrorSnapshot {
        category: "budget_exceeded".to_string(),
        code: Some("budget_exceeded".to_string()),
        reason: Some(kind.to_string()),
        attempt_count: None,
        message: format!(
            "{kind} budget exceeded (cap=${cap:.6}, projected=${projected:.6}, session=${session:.6})"
        ),
        status: None,
    }
}

/// Apply a validated per-step override dict over an already-cloned base
/// [`LlmCallOptions`]. Keys were checked against
/// [`LADDER_STEP_OVERRIDE_KEYS`] at build time, so a present value here is
/// known-supported; type-mismatched values are ignored (the base value
/// stands) rather than erroring mid-dispatch.
pub(super) fn apply_ladder_step_overrides(
    opts: &mut LlmCallOptions,
    overrides: &crate::value::DictMap,
) {
    let as_f64 = |value: &VmValue| -> Option<f64> {
        match value {
            VmValue::Float(f) => Some(*f),
            VmValue::Int(i) => Some(*i as f64),
            _ => None,
        }
    };
    if let Some(v) = overrides.get("temperature").and_then(as_f64) {
        opts.temperature = Some(v);
    }
    if let Some(v) = overrides.get("max_tokens").and_then(VmValue::as_int) {
        opts.max_tokens = v;
    }
    if let Some(v) = overrides.get("top_p").and_then(as_f64) {
        opts.top_p = Some(v);
    }
    if let Some(v) = overrides.get("top_k").and_then(VmValue::as_int) {
        opts.top_k = Some(v);
    }
    if let Some(v) = overrides.get("seed").and_then(VmValue::as_int) {
        opts.seed = Some(v);
    }
    if let Some(v) = overrides.get("frequency_penalty").and_then(as_f64) {
        opts.frequency_penalty = Some(v);
    }
    if let Some(v) = overrides.get("presence_penalty").and_then(as_f64) {
        opts.presence_penalty = Some(v);
    }
    if let Some(v) = overrides.get("timeout_ms").and_then(VmValue::as_int) {
        if v > 0 {
            opts.timeout = Some(((v as u64) / 1000).max(1));
        }
    }
    if let Some(VmValue::Bool(v)) = overrides.get("fast") {
        opts.fast = *v;
    }
}

/// Pre-flight budget check that handles the policy's `on_exceed` knob.
/// Returns `Ok(true)` when the call may proceed, `Ok(false)` when the
/// caller should skip this link and try the next one, and `Err(_)` for
/// abort (caller surfaces the standard budget-exceeded error).
fn check_link_budget(
    policy: &RoutingPolicyConfig,
    opts: &LlmCallOptions,
    dispatch: &str,
    attempt_idx: usize,
    link_label: &str,
    trace_attempts: &mut Vec<RoutingAttempt>,
) -> Result<bool, (VmError, RoutingErrorSnapshot)> {
    let Some(rules_envelope) = policy.budget.envelope() else {
        return Ok(true);
    };
    let session_cost = peek_total_cost();
    let projection = crate::llm::cost::project_llm_call_cost(opts, session_cost);
    let action = policy.budget.on_exceed_or_abort();

    let mut breach = None::<(crate::llm::cost::BudgetLimitKind, f64, &'static str)>;
    if let Some(max) = rules_envelope.max_cost_usd {
        if projection.projected_cost_usd > max {
            breach = Some((
                crate::llm::cost::BudgetLimitKind::PerCallCost,
                max,
                "per_call",
            ));
        }
    }
    if breach.is_none() {
        if let Some(max) = rules_envelope.total_budget_usd {
            if session_cost + projection.projected_cost_usd > max {
                breach = Some((crate::llm::cost::BudgetLimitKind::TotalCost, max, "session"));
            }
        }
    }

    let Some((limit_kind, limit_value, kind_label)) = breach else {
        return Ok(true);
    };

    let snapshot = budget_overrun_snapshot(
        limit_value,
        projection.projected_cost_usd,
        session_cost,
        kind_label,
    );

    let mut meta = serde_json::Map::new();
    meta.insert("policy".to_string(), json!(policy.label.clone()));
    meta.insert("attempt".to_string(), json!(attempt_idx));
    meta.insert("provider".to_string(), json!(opts.provider.clone()));
    meta.insert("model".to_string(), json!(opts.model.clone()));
    meta.insert("link_label".to_string(), json!(link_label));
    meta.insert("kind".to_string(), json!(kind_label));
    meta.insert("limit_usd".to_string(), json!(limit_value));
    meta.insert(
        "projected_cost_usd".to_string(),
        json!(projection.projected_cost_usd),
    );
    meta.insert("session_cost_usd".to_string(), json!(session_cost));
    meta.insert("on_exceed".to_string(), json!(action.as_str()));
    emit_routing_event(dispatch, "budget_exceeded", meta);

    match action {
        BudgetExceedAction::Abort => Err((
            crate::llm::cost::budget_exceeded_error(&projection, limit_kind, limit_value),
            snapshot,
        )),
        BudgetExceedAction::Skip => {
            trace_attempts.push(RoutingAttempt {
                index: attempt_idx,
                provider: opts.provider.clone(),
                model: opts.model.clone(),
                label: link_label.to_string(),
                status: AttemptStatus::Skipped,
                duration_ms: 0,
                cost_usd: None,
                input_tokens: None,
                output_tokens: None,
                error: Some(snapshot),
                verifier_signals: Vec::new(),
                verifier_outcome: None,
            });
            Ok(false)
        }
        BudgetExceedAction::Warn => Ok(true),
    }
}

fn project_link_cost_usd(result: &crate::llm::api::LlmResult) -> f64 {
    calculate_cost_for_provider(
        &result.provider,
        &result.model,
        result.input_tokens,
        result.output_tokens,
    )
}

pub(super) fn duration_ms(elapsed: Duration) -> u64 {
    elapsed.as_millis().try_into().unwrap_or(u64::MAX)
}

/// Lightweight wrapper that observes one provider call. `observed_llm_call`
/// is fail-fast on transient errors, which is exactly what the policy wants:
/// the policy itself owns retry semantics (max_attempts across the chain plus
/// per-link failover rules), so nothing may retry underneath it.
async fn execute_link(
    opts: &LlmCallOptions,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
    delta_sink: Option<crate::llm::api::DeltaSender>,
) -> (Result<crate::llm::api::LlmResult, VmError>, bool) {
    let Some(delta_sink) = delta_sink else {
        return (
            crate::llm::agent_observe::observed_llm_call(
                opts,
                None,
                bridge,
                None,
                false,
                bridge.is_some(),
                None,
                None,
            )
            .await,
            false,
        );
    };

    // Forward deltas while the call is in flight and remember whether public
    // output committed this route. A later route cannot replace visible bytes
    // without splicing two providers into one answer.
    let (attempt_tx, mut attempt_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let mut call = Box::pin(crate::llm::agent_observe::observed_llm_call(
        opts,
        None,
        bridge,
        None,
        false,
        bridge.is_some(),
        None,
        Some(attempt_tx),
    ));
    let mut stream_committed = false;
    let mut deltas_open = true;
    let result = loop {
        tokio::select! {
            maybe_delta = attempt_rx.recv(), if deltas_open => {
                match maybe_delta {
                    Some(delta) => {
                        if delta_sink.send(delta).is_ok() {
                            stream_committed = true;
                        }
                    }
                    None => deltas_open = false,
                }
            }
            result = &mut call => break result,
        }
    };
    while let Ok(delta) = attempt_rx.try_recv() {
        if delta_sink.send(delta).is_ok() {
            stream_committed = true;
        }
    }
    (result, stream_committed)
}

fn pending_attempt_record(
    attempt_no: usize,
    link: &ChainLink,
    label: &str,
    elapsed: Duration,
) -> RoutingAttempt {
    RoutingAttempt {
        index: attempt_no,
        provider: link.provider.clone(),
        model: link.model.clone(),
        label: label.to_string(),
        // Caller patches this to Succeeded on success or attaches the
        // error snapshot for failure.
        status: AttemptStatus::Failed,
        duration_ms: duration_ms(elapsed),
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        error: None,
        verifier_signals: Vec::new(),
        verifier_outcome: None,
    }
}

/// Extract the text the verifier chain should inspect. Mirrors how
/// `agent_config::build_llm_call_result` derives the human-visible
/// answer so the verifier sees the same thing the script will.
fn candidate_text_for_verifier(result: &crate::llm::api::LlmResult) -> String {
    if !result.text.is_empty() {
        return result.text.clone();
    }
    // Tool-only responses still get a deterministic text payload
    // verifiers can match against (their JSON serialization), so
    // `lint`-style pattern rules work on tool-call shape too.
    if !result.tool_calls.is_empty() {
        return serde_json::to_string(&result.tool_calls).unwrap_or_default();
    }
    String::new()
}

/// Run the verifier chain over a candidate. Returns the per-verifier
/// records plus the aggregated outcome. The first non-`accept` signal
/// dominates: a single `escalate` outranks any later `refine`s, so
/// callers don't have to redo the precedence reasoning.
async fn run_verifier_chain(
    verifiers: &[Verifier],
    text: &str,
) -> (Vec<VerifierSignalRecord>, VerifierOutcome, Vec<String>) {
    if verifiers.is_empty() {
        return (Vec::new(), VerifierOutcome::Accept, Vec::new());
    }
    let mut records = Vec::with_capacity(verifiers.len());
    let mut outcome = VerifierOutcome::Accept;
    let mut refine_reasons: Vec<String> = Vec::new();
    for verifier in verifiers {
        let signal = run_verifier(verifier, text).await;
        let signal_label = signal.as_str().to_string();
        let reason = signal.reason().map(str::to_string);
        records.push(VerifierSignalRecord {
            name: verifier.name().to_string(),
            kind: verifier.kind_label().to_string(),
            signal: signal_label,
            reason: reason.clone(),
        });
        match signal {
            VerifierSignal::Accept => {}
            VerifierSignal::Refine { reason } => {
                if outcome == VerifierOutcome::Accept {
                    outcome = VerifierOutcome::Refine;
                }
                refine_reasons.push(reason);
            }
            VerifierSignal::Escalate { .. } => {
                outcome = VerifierOutcome::Escalate;
                // Don't `break` — we still want the full picture for
                // receipts, but no later refine can downgrade escalate.
            }
        }
    }
    (records, outcome, refine_reasons)
}

/// Default `max_attempts` budget when `escalate_on` is configured but
/// the script didn't set one explicitly. Without this nudge, the
/// default `chain.len()` would silently disable refine retries (each
/// link only gets one shot, leaving no slot for a tightened-prompt
/// retry). Adding `chain.len() * max_refines_per_link` keeps the
/// "happy path = once per link" semantics intact while making room
/// for the refine fan-out the verifier chain expects.
fn implied_max_attempts(policy: &RoutingPolicyConfig) -> usize {
    let base = policy.chain.len();
    if policy.escalate_on.is_empty() {
        base
    } else {
        base + base.saturating_mul(policy.max_refines_per_link)
    }
}

fn emit_verifier_signal_event(
    dispatch: &str,
    policy: &RoutingPolicyConfig,
    attempt_no: usize,
    link: &ChainLink,
    outcome: VerifierOutcome,
    signals: &[VerifierSignalRecord],
) {
    let mut meta = serde_json::Map::new();
    meta.insert("policy".to_string(), json!(policy.label.clone()));
    meta.insert("attempt".to_string(), json!(attempt_no));
    meta.insert("provider".to_string(), json!(link.provider.clone()));
    meta.insert("model".to_string(), json!(link.model.clone()));
    meta.insert("link_label".to_string(), json!(link.display_label()));
    meta.insert("outcome".to_string(), json!(outcome.as_str()));
    meta.insert(
        "signals".to_string(),
        serde_json::Value::Array(
            signals
                .iter()
                .map(|s| {
                    json!({
                        "name": s.name,
                        "kind": s.kind,
                        "signal": s.signal,
                        "reason": s.reason,
                    })
                })
                .collect(),
        ),
    );
    emit_routing_event(dispatch, "verifier_signal", meta);
}

/// Run the chain. Each link is tried in order; failover rules decide
/// whether to advance after an error. `latency.race_after_ms`, when
/// set, kicks off the next attempt in parallel and returns whichever
/// finishes first; the loser is cancelled and recorded as `race_lost`.
///
/// When `policy.escalate_on` is non-empty, each successful link's
/// candidate runs through the verifier chain before being returned:
/// `accept` returns, `refine` retries the same link with a nudge
/// (up to `policy.max_refines_per_link` per link), `escalate`
/// advances to the next link. If the verifier rejects the last link
/// and no frontier remains, the rejected candidate is returned anyway
/// — verifiers gate routing, not correctness.
pub(crate) async fn execute_with_routing(
    policy: &RoutingPolicyConfig,
    mut base_opts: LlmCallOptions,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
    delta_sink: Option<crate::llm::api::DeltaSender>,
) -> Result<(crate::llm::api::LlmResult, RoutingTrace), VmError> {
    let dispatch = policy.dispatch_label();
    let mut trace = RoutingTrace {
        label: policy.label.clone(),
        attempts: Vec::new(),
        selected: None,
        session_cost_usd: peek_total_cost(),
    };
    let max_attempts = policy
        .failover
        .max_attempts
        .unwrap_or_else(|| implied_max_attempts(policy));
    if max_attempts == 0 {
        return Err(runtime_error(
            "routing_policy.failover.max_attempts: must be >= 1".to_string(),
        ));
    }
    let mut last_error: Option<VmError> = None;
    let mut last_snapshot: Option<RoutingErrorSnapshot> = None;
    let mut terminal_was_failover_eligible = false;
    let mut attempts_used: usize = 0;
    let original_messages = base_opts.messages.clone();
    // Per-link refine bookkeeping. Both reset on `idx` advance so a
    // refine streak from one link doesn't bleed into the next.
    let mut refines_for_current_link: usize = 0;
    let mut nudge_reasons_for_current_link: Vec<String> = Vec::new();
    // Last verifier-rejected candidate, returned as a fallback when
    // the chain exhausts without an `accept`.
    let mut last_rejected_candidate: Option<(crate::llm::api::LlmResult, usize)> = None;

    let mut decision_meta = serde_json::Map::new();
    decision_meta.insert("policy".to_string(), json!(policy.label.clone()));
    decision_meta.insert("chain_length".to_string(), json!(policy.chain.len()));
    decision_meta.insert("max_attempts".to_string(), json!(max_attempts));
    decision_meta.insert(
        "chain".to_string(),
        serde_json::Value::Array(
            policy
                .chain
                .iter()
                .map(|link| {
                    json!({
                        "provider": link.provider,
                        "model": link.model,
                        "label": link.display_label(),
                    })
                })
                .collect(),
        ),
    );
    emit_routing_event(&dispatch, "decision", decision_meta);

    let mut idx = 0usize;
    while idx < policy.chain.len() && attempts_used < max_attempts {
        let link = policy.chain[idx].clone();
        let link_label = link.display_label();
        let attempt_no = attempts_used + 1;
        let opts = match auth::link_options_with_auth(&base_opts, policy, &link) {
            Ok(opts) => opts,
            Err(snapshot) => {
                let mut meta = serde_json::Map::new();
                meta.insert("policy".to_string(), json!(policy.label.clone()));
                meta.insert("attempt".to_string(), json!(attempt_no));
                meta.insert("provider".to_string(), json!(link.provider.clone()));
                meta.insert("model".to_string(), json!(link.model.clone()));
                meta.insert("link_label".to_string(), json!(link_label.clone()));
                meta.insert("reason".to_string(), json!("missing_credentials"));
                emit_routing_event(&dispatch, "route_unavailable", meta);

                trace.attempts.push(auth::skipped_attempt_record(
                    attempt_no,
                    &link,
                    &link_label,
                    snapshot.clone(),
                ));
                last_snapshot = Some(snapshot);
                terminal_was_failover_eligible = true;
                attempts_used += 1;
                idx += 1;
                continue;
            }
        };

        let mut local_attempts: Vec<RoutingAttempt> = Vec::new();
        match check_link_budget(
            policy,
            &opts,
            &dispatch,
            attempts_used + 1,
            &link_label,
            &mut local_attempts,
        ) {
            Ok(true) => {}
            Ok(false) => {
                trace.attempts.extend(local_attempts);
                idx += 1;
                attempts_used += 1;
                continue;
            }
            Err((err, snapshot)) => {
                trace.attempts.extend(local_attempts);
                last_error = Some(err);
                last_snapshot = Some(snapshot);
                terminal_was_failover_eligible = false;
                break;
            }
        }
        trace.attempts.extend(local_attempts);

        let start = std::time::Instant::now();
        let mut attempt_meta = serde_json::Map::new();
        attempt_meta.insert("policy".to_string(), json!(policy.label.clone()));
        attempt_meta.insert("attempt".to_string(), json!(attempt_no));
        attempt_meta.insert("provider".to_string(), json!(link.provider.clone()));
        attempt_meta.insert("model".to_string(), json!(link.model.clone()));
        attempt_meta.insert("link_label".to_string(), json!(link_label.clone()));
        emit_routing_event(&dispatch, "attempt", attempt_meta);

        let race_after_ms = policy.latency.race_after_ms;
        let primary_timeout_ms = link
            .timeout_ms
            .or(policy.failover.on_timeout_ms)
            .unwrap_or(DEFAULT_RACE_PRIMARY_TIMEOUT_MS);

        let race_outcome = if let Some(race_after) = race_after_ms {
            if idx + 1 < policy.chain.len() && attempts_used + 2 <= max_attempts {
                let backup_link = policy.chain[idx + 1].clone();
                match auth::link_options_with_auth(&base_opts, policy, &backup_link) {
                    Ok(backup_opts) => {
                        let backup_label = backup_link.display_label();
                        // Do not stream deltas from racing attempts: the loser may
                        // emit text before the winner is known. Callers that need an
                        // observational stream still receive the selected result text
                        // through their non-streaming fallback after routing resolves.
                        Some(
                            run_race(
                                &dispatch,
                                policy,
                                attempts_used,
                                &link,
                                &link_label,
                                &opts,
                                bridge,
                                race_after,
                                primary_timeout_ms,
                                backup_label,
                                backup_opts,
                            )
                            .await,
                        )
                    }
                    Err(_) => None,
                }
            } else {
                None
            }
        } else {
            None
        };

        let raced = race_outcome.is_some();
        let (result, mut attempt_records, stream_committed) = if let Some(outcome) = race_outcome {
            (outcome.0, outcome.1, false)
        } else {
            let (result, stream_committed) = execute_link(&opts, bridge, delta_sink.clone()).await;
            (
                result,
                vec![pending_attempt_record(
                    attempt_no,
                    &link,
                    &link_label,
                    start.elapsed(),
                )],
                stream_committed,
            )
        };
        // Each record in `attempt_records` is one chain slot consumed
        // (1 for a normal call, 2 when racing actually started a backup).
        // Counting from the record vec keeps `max_attempts` honest and
        // prevents the chain from re-trying the same backup on the next
        // iteration.
        let consumed = attempt_records.len().max(1);

        match result {
            Ok(value) => {
                // Racing attempts are intentionally unstreamed until a winner
                // is known. Publish only the selected response after routing
                // resolves so callers receive one coherent stream.
                if raced {
                    if let Some(sink) = delta_sink.as_ref() {
                        if !value.text.is_empty() {
                            let _ = sink.send(value.text.clone());
                        }
                    }
                }
                if let Some(record) = attempt_records
                    .iter_mut()
                    .find(|rec| matches!(rec.status, AttemptStatus::Failed) && rec.error.is_none())
                {
                    record.status = AttemptStatus::Succeeded;
                    record.cost_usd = Some(project_link_cost_usd(&value));
                    record.input_tokens = Some(value.input_tokens);
                    record.output_tokens = Some(value.output_tokens);
                }
                // Run the verifier chain over the winning candidate.
                let candidate_text = if policy.escalate_on.is_empty() {
                    String::new()
                } else {
                    candidate_text_for_verifier(&value)
                };
                let (signals, outcome, refine_reasons) =
                    run_verifier_chain(&policy.escalate_on, &candidate_text).await;
                let outcome_for_attempt = if policy.escalate_on.is_empty() {
                    None
                } else {
                    Some(outcome)
                };
                if let Some(record) = attempt_records
                    .iter_mut()
                    .find(|rec| matches!(rec.status, AttemptStatus::Succeeded))
                {
                    record.verifier_signals = signals.clone();
                    record.verifier_outcome = outcome_for_attempt;
                }
                if !policy.escalate_on.is_empty() {
                    emit_verifier_signal_event(
                        &dispatch, policy, attempt_no, &link, outcome, &signals,
                    );
                }
                let starting_len = trace.attempts.len();
                trace.attempts.extend(attempt_records);
                let success_idx = trace
                    .attempts
                    .iter()
                    .enumerate()
                    .skip(starting_len)
                    .find(|(_, a)| {
                        matches!(a.status, AttemptStatus::Succeeded)
                            && a.provider == value.provider
                            && a.model == value.model
                    })
                    .map(|(idx, _)| idx);

                match outcome {
                    VerifierOutcome::Accept => {
                        trace.selected = success_idx;
                        trace.session_cost_usd = peek_total_cost();
                        return Ok((value, trace));
                    }
                    VerifierOutcome::Refine
                        if refines_for_current_link < policy.max_refines_per_link
                            && attempts_used + consumed < max_attempts =>
                    {
                        // Same link, tightened prompt: append the
                        // cumulative refine nudge to the original
                        // messages snapshot (re-applying each iteration
                        // keeps prior bad responses out of context).
                        nudge_reasons_for_current_link.extend(refine_reasons);
                        let nudge = build_refine_nudge(&nudge_reasons_for_current_link);
                        base_opts.messages = original_messages.clone();
                        if !nudge.is_empty() {
                            base_opts.messages.push(serde_json::json!({
                                "role": "user",
                                "content": nudge,
                            }));
                        }
                        refines_for_current_link += 1;
                        attempts_used += consumed;
                        if let Some(idx_v) = success_idx {
                            last_rejected_candidate = Some((value, idx_v));
                        }
                        continue;
                    }
                    VerifierOutcome::Refine | VerifierOutcome::Escalate => {
                        // Either refine budget is exhausted (treat as
                        // escalate) or verifier escalated outright:
                        // advance to the next link, reset link-local
                        // refine state.
                        refines_for_current_link = 0;
                        nudge_reasons_for_current_link.clear();
                        base_opts.messages = original_messages.clone();
                        attempts_used += consumed;
                        idx += consumed;
                        if let Some(idx_v) = success_idx {
                            last_rejected_candidate = Some((value, idx_v));
                        }
                        continue;
                    }
                }
            }
            Err(err) => {
                let (mut eligible, snapshot) = matches_failover(&policy.failover, &err);
                if stream_committed {
                    // Public bytes bind this logical call to the current route.
                    // Advancing would concatenate a backup answer after a
                    // partial primary response.
                    eligible = false;
                    let mut meta = serde_json::Map::new();
                    meta.insert("policy".to_string(), json!(policy.label.clone()));
                    meta.insert("attempt".to_string(), json!(attempt_no));
                    meta.insert("provider".to_string(), json!(link.provider.clone()));
                    meta.insert("model".to_string(), json!(link.model.clone()));
                    meta.insert("reason".to_string(), json!("public_stream_committed"));
                    emit_routing_event(&dispatch, "failover_suppressed", meta);
                }
                terminal_was_failover_eligible = eligible;
                let failure_category = snapshot.category.clone();
                if let Some(record) = attempt_records
                    .iter_mut()
                    .find(|rec| matches!(rec.status, AttemptStatus::Failed) && rec.error.is_none())
                {
                    record.error = Some(snapshot.clone());
                }
                trace.attempts.extend(attempt_records);
                last_snapshot = Some(snapshot);
                attempts_used += consumed;
                if !eligible {
                    last_error = Some(err);
                    break;
                }
                // Model-ladder step advance: emit a dedicated
                // `llm_models_advance` trace event so cost dashboards can see
                // which transport-class failure escalated the ladder and to
                // which rung. Only for ladders; explicit routing policies keep
                // their existing attempt telemetry unchanged. Skipped when the
                // failed rung was the last one (nothing to advance to).
                if policy.is_ladder {
                    if let Some(next_link) = policy.chain.get(idx + consumed) {
                        crate::llm::trace::emit_agent_event(
                            crate::llm::trace::AgentTraceEvent::ModelsAdvance {
                                from_index: idx,
                                from_model: link.model.clone(),
                                to_model: next_link.model.clone(),
                                category: failure_category,
                            },
                        );
                    }
                }
                last_error = Some(err);
                idx += consumed;
                continue;
            }
        }
    }

    // Verifier-rejected fallback: if the chain exhausted because the
    // verifier kept escalating but we never got a transport error,
    // return the last successful candidate the chain produced rather
    // than failing the call. The verifier complaint is preserved on
    // `routing.attempts[*].verifier_outcome` so the caller can still
    // see why escalation ran out.
    if last_error.is_none() {
        if let Some((value, idx)) = last_rejected_candidate {
            trace.selected = Some(idx);
            trace.session_cost_usd = peek_total_cost();
            return Ok((value, trace));
        }
    }

    let err = last_error.unwrap_or_else(|| {
        runtime_error("routing_policy: chain exhausted with no attempts (empty chain?)".to_string())
    });
    let mut meta = serde_json::Map::new();
    meta.insert("policy".to_string(), json!(policy.label.clone()));
    meta.insert("attempts".to_string(), json!(trace.attempts.len()));
    if let Some(snapshot) = last_snapshot.as_ref() {
        meta.insert("last_error_category".to_string(), json!(&snapshot.category));
        meta.insert("last_error_message".to_string(), json!(&snapshot.message));
        if let Some(code) = snapshot.code.as_ref() {
            meta.insert("last_error_code".to_string(), json!(code));
        }
        if let Some(reason) = snapshot.reason.as_ref() {
            meta.insert("last_error_reason".to_string(), json!(reason));
        }
        if let Some(status) = snapshot.status {
            meta.insert("last_error_status".to_string(), json!(status));
        }
    }
    meta.insert(
        "attempt_chain".to_string(),
        crate::llm::helpers::vm_value_to_json(&trace_to_vm_attempts(&trace)),
    );
    emit_routing_event(&dispatch, "exhausted", meta);
    if terminal_was_failover_eligible {
        return Err(provider_exhausted_routing_error(
            &trace,
            last_snapshot.as_ref(),
        ));
    }
    Err(err)
}

pub(super) fn provider_exhausted_routing_error(
    trace: &RoutingTrace,
    last: Option<&RoutingErrorSnapshot>,
) -> VmError {
    let reason = last
        .and_then(|snapshot| snapshot.reason.as_deref())
        .unwrap_or("provider_exhausted");
    let category = last
        .map(|snapshot| snapshot.category.as_str())
        .unwrap_or("generic");
    let request_attempt_count = physical_request_attempt_count(trace);
    let message = format!(
        "provider routes exhausted after {request_attempt_count} request attempt(s) across {} route(s)",
        trace.attempts.len()
    );
    provider_exhausted_error(
        category,
        reason,
        request_attempt_count,
        message,
        trace_to_vm_attempts(trace),
    )
}

pub(super) fn physical_request_attempt_count(trace: &RoutingTrace) -> usize {
    trace
        .attempts
        .iter()
        .map(|attempt| {
            if matches!(attempt.status, AttemptStatus::Skipped) {
                0
            } else {
                attempt
                    .error
                    .as_ref()
                    .and_then(|error| error.attempt_count)
                    .unwrap_or(1)
            }
        })
        .sum()
}

pub(crate) fn provider_exhausted_error(
    category: &str,
    reason: &str,
    attempt_count: usize,
    message: String,
    attempts: VmValue,
) -> VmError {
    VmError::Thrown(VmValue::dict(BTreeMap::from([
        (
            "category".to_string(),
            VmValue::String(arcstr::ArcStr::from(category)),
        ),
        (
            "code".to_string(),
            VmValue::String(arcstr::ArcStr::from("provider_exhausted")),
        ),
        (
            "kind".to_string(),
            VmValue::String(arcstr::ArcStr::from("terminal")),
        ),
        (
            "reason".to_string(),
            VmValue::String(arcstr::ArcStr::from(reason)),
        ),
        (
            "message".to_string(),
            VmValue::String(arcstr::ArcStr::from(message)),
        ),
        (
            "attempt_count".to_string(),
            VmValue::Int(attempt_count as i64),
        ),
        ("attempts".to_string(), attempts),
    ])))
}
