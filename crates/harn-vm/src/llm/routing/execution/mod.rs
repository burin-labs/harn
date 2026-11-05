use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::value::{ErrorCategory, VmDictExt, VmError, VmValue};

use crate::llm::api::LlmCallOptions;
use crate::llm::cost::peek_total_cost;

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
    RoutingTrace, TerminalRoute,
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

#[expect(
    clippy::string_slice,
    reason = "idx comes from find() of ASCII needles on the byte-length-preserving \
              ASCII-lowercased copy of message, so it is a char boundary in message"
)]
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
/// known to be part of the step schema; route capability admission happens
/// after these overrides resolve. Type-mismatched values are ignored (the base
/// value stands) rather than erroring mid-dispatch.
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
        opts.portable_option_intent
            .insert(crate::llm::capabilities::PortableOption::Temperature);
    }
    if let Some(v) = overrides.get("max_tokens").and_then(VmValue::as_int) {
        opts.max_tokens = v;
    }
    if let Some(v) = overrides.get("top_p").and_then(as_f64) {
        opts.top_p = Some(v);
        opts.portable_option_intent
            .insert(crate::llm::capabilities::PortableOption::TopP);
    }
    if let Some(v) = overrides.get("top_k").and_then(VmValue::as_int) {
        opts.top_k = Some(v);
        opts.portable_option_intent
            .insert(crate::llm::capabilities::PortableOption::TopK);
    }
    if let Some(v) = overrides.get("seed").and_then(VmValue::as_int) {
        opts.seed = Some(v);
        opts.portable_option_intent
            .insert(crate::llm::capabilities::PortableOption::Seed);
    }
    if let Some(v) = overrides.get("frequency_penalty").and_then(as_f64) {
        opts.frequency_penalty = Some(v);
        opts.portable_option_intent
            .insert(crate::llm::capabilities::PortableOption::FrequencyPenalty);
    }
    if let Some(v) = overrides.get("presence_penalty").and_then(as_f64) {
        opts.presence_penalty = Some(v);
        opts.portable_option_intent
            .insert(crate::llm::capabilities::PortableOption::PresencePenalty);
    }
    if let Some(v) = overrides.get("timeout_ms").and_then(VmValue::as_int) {
        if v > 0 {
            opts.timeout = Some(((v as u64) / 1000).max(1));
        }
    }
    if let Some(VmValue::String(v)) = overrides.get("speed") {
        opts.fast = v.as_str() == "fast";
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
            });
            Ok(false)
        }
        BudgetExceedAction::Warn => Ok(true),
    }
}

fn project_link_cost_usd(result: &crate::llm::api::LlmResult) -> Option<f64> {
    result.usage().cost_usd
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
    }
}

/// Resolve the authoritative `(provider, model)` of the route a terminal
/// marker names. `Attempt(index)` is matched to its [`RoutingAttempt`] by
/// `index` — never by position, since race records stay in configured order
/// regardless of which future resolved. `Composite` (no single route is
/// responsible) yields `None`; callers must not fabricate a route for it.
fn terminal_route_facts(
    records: &[RoutingAttempt],
    terminal: TerminalRoute,
) -> Option<(String, String)> {
    match terminal {
        TerminalRoute::Attempt(index) => records
            .iter()
            .find(|record| record.index == index)
            .map(|record| (record.provider.clone(), record.model.clone())),
        TerminalRoute::Composite => None,
    }
}

/// Run the chain. Each link is tried in order; failover rules decide
/// whether to advance after an error. `latency.race_after_ms`, when
/// set, kicks off the next attempt in parallel and returns whichever
/// finishes first; the loser is cancelled and recorded as `race_lost`.
///
pub(crate) async fn execute_with_routing(
    policy: &RoutingPolicyConfig,
    base_opts: LlmCallOptions,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
    delta_sink: Option<crate::llm::api::DeltaSender>,
) -> Result<(crate::llm::api::LlmResult, RoutingTrace), VmError> {
    let dispatch = policy.dispatch_label();
    let mut trace = RoutingTrace {
        label: policy.label.clone(),
        attempts: Vec::new(),
        selected: None,
        terminal: None,
        session_cost_usd: peek_total_cost(),
    };
    let max_attempts = policy.failover.max_attempts.unwrap_or(policy.chain.len());
    if max_attempts == 0 {
        return Err(runtime_error(
            "routing_policy.failover.max_attempts: must be >= 1".to_string(),
        ));
    }
    let mut last_error: Option<VmError> = None;
    let mut last_snapshot: Option<RoutingErrorSnapshot> = None;
    // The terminal marker for the attempt that produced the last error, tracked
    // alongside `last_error`/`last_snapshot`. Sourced from the race OUTCOME (or
    // the single attempt), never from the outer/base link, so the terminal error
    // names the route that actually failed. Folded into `trace.terminal` before
    // the exhausted error is built.
    let mut last_terminal: Option<TerminalRoute> = None;
    let mut terminal_was_failover_eligible = false;
    let mut attempts_used: usize = 0;

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
        let mut opts = match auth::link_options_with_auth(&base_opts, policy, &link) {
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
                last_terminal = Some(TerminalRoute::Attempt(attempt_no));
                terminal_was_failover_eligible = true;
                attempts_used += 1;
                idx += 1;
                continue;
            }
        };
        opts.rate_limit_reroute_on_timeout =
            idx + 1 < policy.chain.len() && attempts_used + 1 < max_attempts;

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
                last_terminal = Some(TerminalRoute::Attempt(attempt_no));
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
                    Ok(mut backup_opts) => {
                        backup_opts.rate_limit_reroute_on_timeout =
                            idx + 2 < policy.chain.len() && attempts_used + 2 < max_attempts;
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
        // `iter_terminal` marks which route produced THIS iteration's result:
        // the race outcome's marker when racing (which racer resolved, or
        // `Composite` when both hit the deadline), else the single attempt.
        let (result, mut attempt_records, stream_committed, iter_terminal) = if let Some(outcome) =
            race_outcome
        {
            (outcome.0, outcome.1, false, outcome.2)
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
                TerminalRoute::Attempt(attempt_no),
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
                    record.cost_usd = project_link_cost_usd(&value);
                    record.input_tokens = Some(value.input_tokens);
                    record.output_tokens = Some(value.output_tokens);
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

                trace.selected = success_idx;
                trace.terminal = Some(iter_terminal);
                trace.session_cost_usd = peek_total_cost();
                return Ok((value, trace));
            }
            Err(err) => {
                let (mut eligible, snapshot) = matches_failover(&policy.failover, &err);
                // Attribute this failure to the route that actually produced it
                // (the terminal racer, or the single attempt), not the outer
                // link — they diverge when a backup lost/won the race.
                let (terminal_provider, terminal_model) =
                    terminal_route_facts(&attempt_records, iter_terminal)
                        .unwrap_or_else(|| (link.provider.clone(), link.model.clone()));
                if stream_committed {
                    // Public bytes bind this logical call to the current route.
                    // Advancing would concatenate a backup answer after a
                    // partial primary response.
                    eligible = false;
                    let mut meta = serde_json::Map::new();
                    meta.insert("policy".to_string(), json!(policy.label.clone()));
                    meta.insert("attempt".to_string(), json!(attempt_no));
                    meta.insert("provider".to_string(), json!(terminal_provider.clone()));
                    meta.insert("model".to_string(), json!(terminal_model.clone()));
                    meta.insert("reason".to_string(), json!("public_stream_committed"));
                    emit_routing_event(&dispatch, "failover_suppressed", meta);
                }
                terminal_was_failover_eligible = eligible;
                last_terminal = Some(iter_terminal);
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
                                from_model: terminal_model.clone(),
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

    // Terminal error path: the route that produced the last error is the
    // authoritative terminal route (never the outer/base link).
    trace.terminal = last_terminal;
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
    let base = provider_exhausted_error(
        category,
        reason,
        request_attempt_count,
        message,
        trace_to_vm_attempts(trace),
    );
    apply_terminal_route(base, trace)
}

/// Stamp the authoritative terminal route onto a provider-exhausted error.
///
/// `Attempt(index)` adds the top-level `provider`/`model` of the route that
/// actually produced the terminal failure, so the outer wrapper
/// (`build_llm_error_dict`) does not backfill the base/requested route and the
/// error names the route that failed. `Composite` (no single route is
/// responsible — e.g. both racers hit the deadline) sets `no_single_route` so
/// the wrapper skips the provider/model fill entirely rather than fabricating
/// one. With no terminal marker the error is returned unchanged.
fn apply_terminal_route(err: VmError, trace: &RoutingTrace) -> VmError {
    let (VmError::Thrown(VmValue::Dict(fields)), Some(terminal)) = (&err, trace.terminal) else {
        return err;
    };
    let mut dict = fields.as_ref().clone();
    match terminal {
        TerminalRoute::Attempt(index) => {
            if let Some(attempt) = trace.attempts.iter().find(|a| a.index == index) {
                dict.put_str("provider", attempt.provider.clone());
                dict.put_str("model", attempt.model.clone());
            }
        }
        TerminalRoute::Composite => {
            dict.put_bool("no_single_route", true);
        }
    }
    VmError::Thrown(VmValue::dict(dict))
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
    VmError::Thrown(VmValue::dict([
        ("category", VmValue::string(category)),
        ("code", VmValue::string("provider_exhausted")),
        ("kind", VmValue::string("terminal")),
        ("reason", VmValue::string(reason)),
        ("message", VmValue::string(message)),
        ("attempt_count", VmValue::Int(attempt_count as i64)),
        ("attempts", attempts),
    ]))
}
