use crate::value::VmDictExt;
use rust_decimal::Decimal;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::str::FromStr;

use crate::stdlib::macros::harn_builtin;
use crate::value::{categorized_error, DictMap, ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

thread_local! {
    static LLM_BUDGET: RefCell<Option<f64>> = const { RefCell::new(None) };
    static LLM_ACCUMULATED_COST: RefCell<f64> = const { RefCell::new(0.0) };
    static LLM_TOKEN_BUDGET: RefCell<Option<u64>> = const { RefCell::new(None) };
    static LLM_ACCUMULATED_TOKENS: RefCell<u64> = const { RefCell::new(0) };
    static LLM_OBSERVED_USAGE: RefCell<ObservedSessionUsage> =
        const { RefCell::new(ObservedSessionUsage::EMPTY) };
}

/// Session-observed token usage, accumulated from completed calls on this
/// thread. The pre-call budget projection reads it so the second and later
/// calls of a session are priced against what this session actually costs
/// (cache hits included) instead of the uncached worst case.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ObservedSessionUsage {
    pub calls: u64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
}

impl ObservedSessionUsage {
    const EMPTY: Self = Self {
        calls: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };

    /// Mean output tokens per observed call, rounded up. `None` before the
    /// first call, which is what keeps the first projection worst-case.
    fn mean_output_tokens(&self) -> Option<i64> {
        if self.calls == 0 {
            return None;
        }
        Some((self.output_tokens.max(0) as f64 / self.calls as f64).ceil() as i64)
    }
}

/// The usage this session has actually observed so far on this thread.
pub(crate) fn peek_observed_session_usage() -> ObservedSessionUsage {
    LLM_OBSERVED_USAGE.with(|usage| *usage.borrow())
}

fn record_observed_session_usage(usage: &crate::llm::usage::LlmUsage) {
    LLM_OBSERVED_USAGE.with(|slot| {
        let mut slot = slot.borrow_mut();
        slot.calls = slot.calls.saturating_add(1);
        slot.input_tokens = slot.input_tokens.saturating_add(usage.input_tokens.max(0));
        slot.output_tokens = slot
            .output_tokens
            .saturating_add(usage.output_tokens.max(0));
        slot.cache_read_tokens = slot
            .cache_read_tokens
            .saturating_add(usage.cache_read_tokens.max(0));
        slot.cache_write_tokens = slot
            .cache_write_tokens
            .saturating_add(usage.cache_write_tokens.max(0));
    });
}

/// Reset thread-local cost state. Call between test runs to avoid leaking.
pub(crate) fn reset_cost_state() {
    LLM_BUDGET.with(|b| *b.borrow_mut() = None);
    LLM_ACCUMULATED_COST.with(|a| *a.borrow_mut() = 0.0);
    LLM_TOKEN_BUDGET.with(|b| *b.borrow_mut() = None);
    LLM_ACCUMULATED_TOKENS.with(|a| *a.borrow_mut() = 0);
    LLM_OBSERVED_USAGE.with(|u| *u.borrow_mut() = ObservedSessionUsage::EMPTY);
}

pub fn peek_total_cost() -> f64 {
    LLM_ACCUMULATED_COST.with(|acc| *acc.borrow())
}

/// RAII guard installed by [`install_llm_cost_budget`]. Restores the
/// prior ceiling (and accumulated total) on drop so nested dispatches
/// (a handler that re-enters the dispatcher) cannot leak a tighter
/// budget into the outer scope, or a wider one back into a finished
/// inner scope.
#[must_use = "dropping the guard immediately restores the prior LLM cost budget"]
pub struct LlmBudgetGuard {
    previous_budget: Option<f64>,
    previous_accumulated: f64,
    previous_observed: ObservedSessionUsage,
}

impl Drop for LlmBudgetGuard {
    fn drop(&mut self) {
        LLM_BUDGET.with(|b| *b.borrow_mut() = self.previous_budget);
        LLM_ACCUMULATED_COST.with(|a| *a.borrow_mut() = self.previous_accumulated);
        LLM_OBSERVED_USAGE.with(|u| *u.borrow_mut() = self.previous_observed);
    }
}

/// Pin the per-call LLM cost ceiling at `max_cost_usd` for the lifetime
/// of the returned guard. Sourced from `@budget(llm_cost_usd = …)` on
/// `.harn` handlers in `harn-serve`; mid-call exhaustion raises a
/// `BudgetExceeded`-categorised error which adapter codecs render as
/// HTTP 429.
pub fn install_llm_cost_budget(max_cost_usd: f64) -> LlmBudgetGuard {
    let previous_budget = LLM_BUDGET.with(|b| b.borrow().to_owned());
    let previous_accumulated = LLM_ACCUMULATED_COST.with(|a| *a.borrow());
    let previous_observed = peek_observed_session_usage();
    LLM_BUDGET.with(|b| *b.borrow_mut() = Some(max_cost_usd.max(0.0)));
    LLM_ACCUMULATED_COST.with(|a| *a.borrow_mut() = 0.0);
    LLM_OBSERVED_USAGE.with(|u| *u.borrow_mut() = ObservedSessionUsage::EMPTY);
    LlmBudgetGuard {
        previous_budget,
        previous_accumulated,
        previous_observed,
    }
}

/// RAII guard for [`install_llm_token_budget`]. Pairs the dispatch-level
/// token cap with the cost-cap guard so `@budget(llm_tokens, llm_cost_usd)`
/// both restore on drop.
#[must_use = "dropping the guard immediately restores the prior LLM token budget"]
pub struct LlmTokenBudgetGuard {
    previous_budget: Option<u64>,
    previous_accumulated: u64,
}

impl Drop for LlmTokenBudgetGuard {
    fn drop(&mut self) {
        LLM_TOKEN_BUDGET.with(|b| *b.borrow_mut() = self.previous_budget);
        LLM_ACCUMULATED_TOKENS.with(|a| *a.borrow_mut() = self.previous_accumulated);
    }
}

/// Pin the per-dispatch LLM token ceiling (input + output combined) at
/// `max_tokens` for the lifetime of the returned guard. Sourced from
/// `@budget(llm_tokens: …)` on `.harn` handlers in `harn-serve`. Like
/// the cost-cap variant, mid-stream exhaustion raises a
/// `BudgetExceeded`-categorised error that adapters render as HTTP 429.
pub fn install_llm_token_budget(max_tokens: u64) -> LlmTokenBudgetGuard {
    let previous_budget = LLM_TOKEN_BUDGET.with(|b| *b.borrow());
    let previous_accumulated = LLM_ACCUMULATED_TOKENS.with(|a| *a.borrow());
    LLM_TOKEN_BUDGET.with(|b| *b.borrow_mut() = Some(max_tokens));
    LLM_ACCUMULATED_TOKENS.with(|a| *a.borrow_mut() = 0);
    LlmTokenBudgetGuard {
        previous_budget,
        previous_accumulated,
    }
}

pub fn peek_total_tokens() -> u64 {
    LLM_ACCUMULATED_TOKENS.with(|acc| *acc.borrow())
}

/// Re-arm the live per-thread LLM **cost** ceiling in place, preserving the
/// accumulated total. `None` clears the cap.
///
/// Unlike [`install_llm_cost_budget`] this returns no guard and does not reset
/// the running total: it mutates the same `LLM_BUDGET` thread-local a dispatch
/// already consults at preflight ([`check_llm_preflight_budget`]) and after
/// each call ([`record_llm_usage`]). A supervisor on the dispatch
/// thread can therefore tighten or loosen the ceiling mid-run and have the next
/// LLM call observe it — the basis for ACP `session/set_budget` re-arm
/// in a downstream host. Callers that want fresh per-scope accounting
/// (HTTP `@budget`, per-turn guards) keep using `install_*` instead.
pub fn set_llm_cost_budget(max_cost_usd: Option<f64>) {
    LLM_BUDGET.with(|b| *b.borrow_mut() = max_cost_usd.map(|max| max.max(0.0)));
}

/// Re-arm the live per-thread LLM **token** ceiling in place, preserving the
/// accumulated total. `None` clears the cap. The token counterpart to
/// [`set_llm_cost_budget`] — see that function for the re-arm semantics.
pub fn set_llm_token_budget(max_tokens: Option<u64>) {
    LLM_TOKEN_BUDGET.with(|b| *b.borrow_mut() = max_tokens);
}

/// The live per-thread LLM cost ceiling, or `None` when uncapped. Pairs with
/// [`peek_total_cost`] so a supervisor (or a re-arm acknowledgement) can read
/// back the ceiling it just set.
pub fn peek_llm_cost_budget() -> Option<f64> {
    LLM_BUDGET.with(|b| *b.borrow())
}

/// The live per-thread LLM token ceiling, or `None` when uncapped. The token
/// counterpart to [`peek_llm_cost_budget`].
pub fn peek_llm_token_budget() -> Option<u64> {
    LLM_TOKEN_BUDGET.with(|b| *b.borrow())
}

// `Serialize` lets the typed-options parity test compare this default key set
// with `LlmBudget` in `std/llm/options.harn`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub(crate) struct LlmBudgetEnvelope {
    pub max_cost_usd: Option<f64>,
    pub total_budget_usd: Option<f64>,
    pub max_input_tokens: Option<i64>,
    pub max_output_tokens: Option<i64>,
}

impl LlmBudgetEnvelope {
    pub(crate) fn is_empty(&self) -> bool {
        self.max_cost_usd.is_none()
            && self.total_budget_usd.is_none()
            && self.max_input_tokens.is_none()
            && self.max_output_tokens.is_none()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LlmBudgetProjection {
    pub provider: String,
    pub model: String,
    pub projected_input_tokens: i64,
    /// The request's output budget (`max_tokens`). Token-ceiling checks and
    /// rate-limit reservations keep using this worst case even when the cost
    /// projection prices a smaller, observed output.
    pub projected_output_tokens: i64,
    /// Output tokens the `projected_cost_usd` figure was priced from. Equal to
    /// `projected_output_tokens` on a worst-case projection; the session's
    /// observed mean output per call (clamped to the budget) otherwise.
    pub costed_output_tokens: i64,
    pub projected_cost_usd: f64,
    pub session_cost_usd: f64,
    pub basis: ProjectionBasis,
}

/// What the pre-call cost projection was computed from. A reader who sees a
/// `budget_exceeded` stop needs this to tell "the session spent the cap" from
/// "the next call's *estimate* crossed the cap while actual spend was well
/// under it".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionBasis {
    /// No completed call in this session yet: every projected input token is
    /// priced uncached and the whole output budget is assumed spent.
    WorstCase,
    /// Priced from this session's observed cache-hit ratio and mean output
    /// tokens per call.
    Observed,
}

impl ProjectionBasis {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ProjectionBasis::WorstCase => "worst_case",
            ProjectionBasis::Observed => "observed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub(crate) struct LlmContextTokenBreakdown {
    pub schema: &'static str,
    pub segments: Vec<LlmContextTokenSegment>,
    pub input_tokens: i64,
    pub output_budget_tokens: i64,
    pub context_tokens: i64,
    pub message_count: usize,
    pub native_tool_count: usize,
    pub provider_tool_count: usize,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub(crate) struct LlmContextTokenSegment {
    pub id: &'static str,
    pub label: &'static str,
    pub tokens: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BudgetLimitKind {
    PerCallCost,
    TotalCost,
    InputTokens,
    OutputTokens,
}

impl BudgetLimitKind {
    fn as_str(self) -> &'static str {
        match self {
            BudgetLimitKind::PerCallCost => "max_cost_usd",
            BudgetLimitKind::TotalCost => "total_budget_usd",
            BudgetLimitKind::InputTokens => "max_input_tokens",
            BudgetLimitKind::OutputTokens => "max_output_tokens",
        }
    }
}

fn numeric_value(value: &VmValue, key: &str) -> Result<f64, VmError> {
    let value = match value {
        VmValue::Float(f) => *f,
        VmValue::Int(n) => *n as f64,
        _ => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!("budget.{key}: expected a non-negative number"),
            ))));
        }
    };
    if !value.is_finite() || value < 0.0 {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("budget.{key}: expected a non-negative finite number"),
        ))));
    }
    Ok(value)
}

fn integer_value(value: &VmValue, key: &str) -> Result<i64, VmError> {
    let value = match value {
        VmValue::Int(n) => *n,
        VmValue::Float(f) if f.is_finite() && f.fract() == 0.0 => *f as i64,
        _ => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!("budget.{key}: expected a non-negative integer"),
            ))));
        }
    };
    if value < 0 {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("budget.{key}: expected a non-negative integer"),
        ))));
    }
    Ok(value)
}

fn parse_budget_fields(
    fields: &crate::value::DictMap,
    envelope: &mut LlmBudgetEnvelope,
) -> Result<(), VmError> {
    if let Some(value) = fields.get("max_cost_usd") {
        envelope.max_cost_usd = Some(numeric_value(value, "max_cost_usd")?);
    }
    if let Some(value) = fields.get("total_budget_usd") {
        envelope.total_budget_usd = Some(numeric_value(value, "total_budget_usd")?);
    }
    if let Some(value) = fields.get("max_input_tokens") {
        envelope.max_input_tokens = Some(integer_value(value, "max_input_tokens")?);
    }
    if let Some(value) = fields.get("max_output_tokens") {
        envelope.max_output_tokens = Some(integer_value(value, "max_output_tokens")?);
    }
    Ok(())
}

pub(crate) fn parse_budget(
    options: Option<&DictMap>,
) -> Result<Option<LlmBudgetEnvelope>, VmError> {
    let Some(options) = options else {
        return Ok(None);
    };
    let mut envelope = LlmBudgetEnvelope::default();
    if let Some(value) = options.get("budget") {
        match value {
            VmValue::Nil => {}
            VmValue::Dict(fields) => parse_budget_fields(fields, &mut envelope)?,
            VmValue::Int(_) | VmValue::Float(_) => {
                envelope.max_cost_usd = Some(numeric_value(value, "budget")?);
            }
            _ => {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    "budget: expected a number (max_cost_usd) or a dict {max_cost_usd?, total_budget_usd?, max_input_tokens?, max_output_tokens?}",
                ))));
            }
        }
    }
    Ok((!envelope.is_empty()).then_some(envelope))
}

pub(super) fn estimate_json_tokens(value: &serde_json::Value, model: &str) -> i64 {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => 1,
        serde_json::Value::String(s) => estimate_text_tokens_for_model(s, model),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| estimate_json_tokens(item, model))
            .sum(),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(key, value)| {
                estimate_text_tokens_for_model(key, model) + estimate_json_tokens(value, model)
            })
            .sum(),
    }
}

fn estimate_text_tokens_for_model(text: &str, model: &str) -> i64 {
    super::token_count::estimate_text_tokens(text, Some(model)).tokens
}

pub(crate) fn project_llm_call_tokens(opts: &super::api::LlmCallOptions) -> (i64, i64) {
    let breakdown = project_llm_call_context_breakdown(opts);
    (breakdown.input_tokens, breakdown.output_budget_tokens)
}

pub(crate) fn project_llm_call_context_breakdown(
    opts: &super::api::LlmCallOptions,
) -> LlmContextTokenBreakdown {
    let system_tokens = opts
        .system
        .as_deref()
        .map(|system| estimate_text_tokens_for_model(system, &opts.model))
        .unwrap_or(0);
    let mut user_message_tokens = 0;
    let mut assistant_message_tokens = 0;
    let mut tool_result_tokens = 0;
    let mut other_message_tokens = 0;
    for message in &opts.messages {
        let tokens = estimate_json_tokens(message, &opts.model);
        if super::context_breakdown::message_is_tool_result(message) {
            tool_result_tokens += tokens;
            continue;
        }
        match message.get("role").and_then(serde_json::Value::as_str) {
            Some("user") => user_message_tokens += tokens,
            Some("assistant") => assistant_message_tokens += tokens,
            _ => other_message_tokens += tokens,
        }
    }
    let tool_tokens: i64 = opts
        .native_tools
        .as_ref()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| {
                    estimate_text_tokens_for_model(
                        &serde_json::to_string(tool).unwrap_or_default(),
                        &opts.model,
                    )
                })
                .sum()
        })
        .unwrap_or(0);
    let provider_tool_tokens: i64 = opts
        .provider_tools
        .iter()
        .map(|tool| {
            estimate_text_tokens_for_model(
                &serde_json::to_string(tool).unwrap_or_default(),
                &opts.model,
            )
        })
        .sum();
    let projected_input_tokens = system_tokens
        .saturating_add(user_message_tokens)
        .saturating_add(assistant_message_tokens)
        .saturating_add(tool_result_tokens)
        .saturating_add(other_message_tokens)
        .saturating_add(tool_tokens)
        .saturating_add(provider_tool_tokens);
    let projected_output_tokens = opts.max_tokens.max(0);
    let segments = vec![
        LlmContextTokenSegment {
            id: "system_prompt",
            label: "System prompt",
            tokens: system_tokens,
        },
        LlmContextTokenSegment {
            id: "user_messages",
            label: "User turns",
            tokens: user_message_tokens,
        },
        LlmContextTokenSegment {
            id: "assistant_messages",
            label: "Assistant turns",
            tokens: assistant_message_tokens,
        },
        LlmContextTokenSegment {
            id: "tool_results",
            label: "Tool results",
            tokens: tool_result_tokens,
        },
        LlmContextTokenSegment {
            id: "other_messages",
            label: "Other messages",
            tokens: other_message_tokens,
        },
        LlmContextTokenSegment {
            id: "native_tool_schemas",
            label: "Native tool schemas",
            tokens: tool_tokens,
        },
        LlmContextTokenSegment {
            id: "provider_tools",
            label: "Provider-hosted tools",
            tokens: provider_tool_tokens,
        },
        LlmContextTokenSegment {
            id: "output_budget",
            label: "Output budget",
            tokens: projected_output_tokens,
        },
    ];
    LlmContextTokenBreakdown {
        schema: "harn.llm.context_token_breakdown.v1",
        segments,
        input_tokens: projected_input_tokens,
        output_budget_tokens: projected_output_tokens,
        context_tokens: projected_input_tokens.saturating_add(projected_output_tokens),
        message_count: opts.messages.len(),
        native_tool_count: opts
            .native_tools
            .as_ref()
            .map(|tools| tools.len())
            .unwrap_or(0),
        provider_tool_count: opts.provider_tools.len(),
    }
}

pub(crate) fn project_llm_call_context_tokens(opts: &super::api::LlmCallOptions) -> u64 {
    let (input, output) = project_llm_call_tokens(opts);
    input.max(0) as u64 + output.max(0) as u64
}

/// Project what the next provider call will cost, in USD.
///
/// Before any call completes there is no evidence, so the projection is the
/// uncached worst case: every projected input token at the full input rate and
/// the entire output budget (`max_tokens`) at the output rate. On a cached,
/// short-answer session that overstates a real call by more than an order of
/// magnitude, which stops a session at a fraction of its cap.
///
/// From the second call on, this session's own completed calls are the
/// evidence: the observed cache-hit ratio splits the projected input into a
/// cache-read-priced share and an uncached remainder, and the observed mean
/// output tokens per call replaces the full output budget (clamped to that
/// budget, so the estimate is never larger than the worst case).
///
/// **Overrun guarantee.** This is a *pre-call* check: it runs only while
/// accumulated session spend is still at or under the cap, and the call it
/// admits is priced from real usage afterwards. So however the projection is
/// computed, actual spend can exceed the limit by at most the true cost of one
/// admitted call. Using observed evidence changes which calls are admitted, not
/// that bound.
pub(crate) fn project_llm_call_cost(
    opts: &super::api::LlmCallOptions,
    session_cost_usd: f64,
) -> LlmBudgetProjection {
    let (projected_input_tokens, projected_output_tokens) = project_llm_call_tokens(opts);
    let output_budget_tokens = projected_output_tokens;
    let observed = peek_observed_session_usage();
    let (costed_output_tokens, projected_cost_usd, basis) = match observed.mean_output_tokens() {
        Some(mean_output_tokens) => {
            let costed_output_tokens = mean_output_tokens.clamp(0, output_budget_tokens);
            let ratio = cache_hit_ratio(
                observed.input_tokens,
                observed.cache_read_tokens,
                observed.cache_write_tokens,
            )
            .clamp(0.0, 1.0);
            let cache_read_tokens = (projected_input_tokens.max(0) as f64 * ratio).round() as i64;
            // `pricing_aware_call_cost_with_cache` treats a cache count
            // that fits inside `input_tokens` as a subset of it, so the
            // uncached remainder is priced at the input rate and the
            // cached share at the cache-read rate.
            let cost = pricing_aware_call_cost_with_cache(
                &opts.provider,
                &opts.model,
                projected_input_tokens,
                costed_output_tokens,
                cache_read_tokens,
                0,
            );
            match cost {
                Some(cost) => (costed_output_tokens, cost, ProjectionBasis::Observed),
                // Unknown pricing: keep the historical zero-cost budget
                // arithmetic rather than inventing a rate.
                None => (
                    costed_output_tokens,
                    calculate_cost_for_provider(
                        &opts.provider,
                        &opts.model,
                        projected_input_tokens,
                        costed_output_tokens,
                    ),
                    ProjectionBasis::Observed,
                ),
            }
        }
        None => (
            output_budget_tokens,
            calculate_cost_for_provider(
                &opts.provider,
                &opts.model,
                projected_input_tokens,
                output_budget_tokens,
            ),
            ProjectionBasis::WorstCase,
        ),
    };
    LlmBudgetProjection {
        provider: opts.provider.clone(),
        model: opts.model.clone(),
        projected_input_tokens,
        projected_output_tokens,
        costed_output_tokens,
        projected_cost_usd,
        session_cost_usd,
        basis,
    }
}

pub(crate) fn budget_exceeded_error(
    projection: &LlmBudgetProjection,
    limit_kind: BudgetLimitKind,
    limit_value: f64,
) -> VmError {
    let mut dict = BTreeMap::new();
    dict.put_str("category", "budget_exceeded");
    dict.put_str("kind", "terminal");
    dict.put_str("reason", "budget_exceeded");
    dict.put_str("limit", limit_kind.as_str());
    dict.insert("limit_value".to_string(), VmValue::Float(limit_value));
    dict.insert(
        "projected_cost_usd".to_string(),
        VmValue::Float(projection.projected_cost_usd),
    );
    dict.insert(
        "session_cost_usd".to_string(),
        VmValue::Float(projection.session_cost_usd),
    );
    dict.insert(
        "projected_input_tokens".to_string(),
        VmValue::Int(projection.projected_input_tokens),
    );
    dict.insert(
        "projected_output_tokens".to_string(),
        VmValue::Int(projection.projected_output_tokens),
    );
    dict.insert(
        "costed_output_tokens".to_string(),
        VmValue::Int(projection.costed_output_tokens),
    );
    // A projection stop is not the same event as spending the cap. These three
    // fields let a reader tell them apart without re-deriving the arithmetic.
    dict.put_str("projection_basis", projection.basis.as_str());
    if matches!(limit_kind, BudgetLimitKind::TotalCost) {
        dict.insert(
            "headroom_usd".to_string(),
            VmValue::Float(limit_value - projection.session_cost_usd),
        );
    }
    dict.put_str("provider", projection.provider.clone());
    dict.put_str("model", projection.model.clone());
    dict.put_str(
        "message",
        format!(
            "LLM budget exceeded before provider call: {} would exceed {}",
            match limit_kind {
                BudgetLimitKind::PerCallCost =>
                    format!("projected cost ${:.6}", projection.projected_cost_usd),
                BudgetLimitKind::TotalCost => format!(
                    "projected session cost ${:.6} (spent ${:.6} of ${:.6}; next call projected \
                     at ${:.6}, {} basis)",
                    projection.session_cost_usd + projection.projected_cost_usd,
                    projection.session_cost_usd,
                    limit_value,
                    projection.projected_cost_usd,
                    projection.basis.as_str(),
                ),
                BudgetLimitKind::InputTokens => format!(
                    "projected input tokens {}",
                    projection.projected_input_tokens
                ),
                BudgetLimitKind::OutputTokens => format!(
                    "projected output tokens {}",
                    projection.projected_output_tokens
                ),
            },
            limit_kind.as_str(),
        ),
    );
    VmError::Thrown(VmValue::dict(dict))
}

pub(crate) fn budget_exceeded_limit(
    envelope: &LlmBudgetEnvelope,
    projection: &LlmBudgetProjection,
) -> Option<(BudgetLimitKind, f64)> {
    if let Some(max) = envelope.max_input_tokens {
        if projection.projected_input_tokens > max {
            return Some((BudgetLimitKind::InputTokens, max as f64));
        }
    }
    if let Some(max) = envelope.max_output_tokens {
        if projection.projected_output_tokens > max {
            return Some((BudgetLimitKind::OutputTokens, max as f64));
        }
    }
    if let Some(max) = envelope.max_cost_usd {
        if projection.projected_cost_usd > max {
            return Some((BudgetLimitKind::PerCallCost, max));
        }
    }
    if let Some(max) = envelope.total_budget_usd {
        if projection.session_cost_usd + projection.projected_cost_usd > max {
            return Some((BudgetLimitKind::TotalCost, max));
        }
    }
    None
}

pub(crate) fn check_budget_envelope(
    envelope: &LlmBudgetEnvelope,
    projection: &LlmBudgetProjection,
) -> Result<(), VmError> {
    if let Some((kind, limit)) = budget_exceeded_limit(envelope, projection) {
        return Err(budget_exceeded_error(projection, kind, limit));
    }
    Ok(())
}

pub(crate) fn check_llm_preflight_budget(
    opts: &super::api::LlmCallOptions,
) -> Result<LlmBudgetProjection, VmError> {
    let session_cost_usd = peek_total_cost();
    let projection = project_llm_call_cost(opts, session_cost_usd);
    if let Some(envelope) = opts.budget.as_ref() {
        check_budget_envelope(envelope, &projection)?;
    }
    LLM_BUDGET.with(|budget| {
        if let Some(max) = *budget.borrow() {
            if session_cost_usd + projection.projected_cost_usd > max {
                return Err(budget_exceeded_error(
                    &projection,
                    BudgetLimitKind::TotalCost,
                    max,
                ));
            }
        }
        Ok(())
    })?;
    Ok(projection)
}

/// Resolved pricing for a (provider, model) pair, expressed per 1k tokens.
/// The `source` discriminates how the rate was found so callers (CLI cost
/// explanation, economics helpers, `cost_route` summaries) can report it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PricingDetail {
    pub input_per_1k: f64,
    pub output_per_1k: f64,
    pub cache_read_per_1k: Option<f64>,
    pub cache_write_per_1k: Option<f64>,
    pub source: PricingSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PricingSource {
    /// Exact model entry in the catalog (configured `[llm.models.<id>]`).
    CatalogModel,
    /// The model's accelerated-serving tier (`serving_tiers[].pricing`), used
    /// when the provider confirmed it served the request fast.
    CatalogServingTier,
    /// Provider-level catalog economics (`[llm.providers.<name>]`).
    ProviderEconomics,
}

impl PricingSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PricingSource::CatalogModel => "catalog_model",
            PricingSource::CatalogServingTier => "catalog_serving_tier",
            PricingSource::ProviderEconomics => "provider_economics",
        }
    }
}

/// Resolve catalog pricing for the route identity reported by a transport.
fn model_pricing_for_observed_route(
    provider: &str,
    model: &str,
) -> Option<crate::llm_config::ModelPricing> {
    crate::llm_config::model_pricing_per_mtok_for_route(provider, model).or_else(|| {
        // Mock responses carry the modeled provider's model identity while the
        // transport remains `mock`. Preserve catalog-backed budget accounting
        // without weakening provider scoping for any real route.
        (provider == "mock")
            .then(|| crate::llm_config::model_pricing_per_mtok(model))
            .flatten()
    })
}

/// Resolve full pricing detail for a (provider, model) pair. Prefers the
/// provider-scoped catalog entry, then falls back to provider economics.
/// Returns `None` for unknown pricing — callers must decide whether to
/// surface that explicitly or coerce to 0.0.
pub(crate) fn pricing_detail_for(provider: &str, model: &str) -> Option<PricingDetail> {
    if let Some(pricing) = model_pricing_for_observed_route(provider, model) {
        return Some(PricingDetail {
            input_per_1k: pricing.input_per_mtok / 1000.0,
            output_per_1k: pricing.output_per_mtok / 1000.0,
            cache_read_per_1k: pricing.cache_read_per_mtok.map(|rate| rate / 1000.0),
            cache_write_per_1k: pricing.cache_write_per_mtok.map(|rate| rate / 1000.0),
            source: PricingSource::CatalogModel,
        });
    }
    let (input, output, _) = crate::llm_config::provider_economics(provider);
    match (input, output) {
        (Some(input_per_1k), Some(output_per_1k)) => Some(PricingDetail {
            input_per_1k,
            output_per_1k,
            cache_read_per_1k: None,
            cache_write_per_1k: None,
            source: PricingSource::ProviderEconomics,
        }),
        _ => None,
    }
}

fn pricing_detail_for_usage(
    provider: &str,
    model: &str,
    input_tokens: i64,
) -> Option<PricingDetail> {
    if let Some(pricing) = model_pricing_for_observed_route(provider, model)
        .map(|pricing| pricing.for_input_tokens(input_tokens))
    {
        return Some(PricingDetail {
            input_per_1k: pricing.input_per_mtok / 1000.0,
            output_per_1k: pricing.output_per_mtok / 1000.0,
            cache_read_per_1k: pricing.cache_read_per_mtok.map(|rate| rate / 1000.0),
            cache_write_per_1k: pricing.cache_write_per_mtok.map(|rate| rate / 1000.0),
            source: PricingSource::CatalogModel,
        });
    }
    pricing_detail_for(provider, model)
}

pub(crate) fn pricing_per_1k_for(provider: &str, model: &str) -> Option<(f64, f64)> {
    pricing_detail_for(provider, model).map(|p| (p.input_per_1k, p.output_per_1k))
}

/// Resolve pricing for a (provider, model) pair, billing at the premium
/// accelerated-serving tier when `served_fast` is set and the catalog declares
/// explicit tier rates or an economic multiplier. Falls back to standard
/// pricing when the request was served at the standard tier, such as after a
/// capacity downgrade.
pub(crate) fn pricing_detail_for_tier(
    provider: &str,
    model: &str,
    served_fast: bool,
    input_tokens: i64,
) -> Option<PricingDetail> {
    if served_fast {
        if let Some(mut pricing) = crate::llm_config::model_serving_tier_pricing_per_mtok_for_route(
            provider,
            model,
            crate::llm::serving_tiers::FAST_TIER_ID,
        ) {
            if let Some(model_pricing) =
                crate::llm_config::model_pricing_per_mtok_for_route(provider, model)
            {
                pricing.input_token_bands = model_pricing.input_token_bands;
            }
            let pricing = pricing.for_input_tokens(input_tokens);
            return Some(PricingDetail {
                input_per_1k: pricing.input_per_mtok / 1000.0,
                output_per_1k: pricing.output_per_mtok / 1000.0,
                cache_read_per_1k: pricing.cache_read_per_mtok.map(|rate| rate / 1000.0),
                cache_write_per_1k: pricing.cache_write_per_mtok.map(|rate| rate / 1000.0),
                source: PricingSource::CatalogServingTier,
            });
        }
    }
    pricing_detail_for_usage(provider, model, input_tokens)
}

pub(crate) fn latency_p50_ms_for(provider: &str) -> Option<u64> {
    let (_, _, latency) = crate::llm_config::provider_economics(provider);
    latency
}

/// Recover the *authored* decimal value of a catalog rate that was parsed
/// from a TOML float literal. The pricing catalog
/// (`crates/harn-vm/src/llm/providers.toml`) writes short, human-authored
/// decimals like `input_per_mtok = 0.15`; TOML deserializes them to `f64`,
/// which cannot store `0.15` exactly. Rust's `{}` float formatter emits the
/// *shortest* decimal string that round-trips to the same `f64` — for these
/// short literals that is exactly the digits the author wrote (`"0.15"`,
/// not `0.150000000000000008…`). Parsing that string straight into a
/// `Decimal` therefore reconstructs the intended exact value without ever
/// laundering the float's binary rounding error into false precision (which
/// `Decimal::from_f64_retain` would). The `from_f64_retain` fallback only
/// fires for non-finite inputs, which the catalog never contains.
fn authored_rate_decimal(rate: f64) -> Decimal {
    Decimal::from_str(&format!("{rate}"))
        .ok()
        .or_else(|| Decimal::from_f64_retain(rate))
        .unwrap_or(Decimal::ZERO)
}

/// Compute the per-call USD cost for a model and token counts as an exact
/// `Decimal`. Sources each rate directly from the per-MTok catalog literal
/// via [`authored_rate_decimal`] (never through the derived per-1k rate,
/// which would round-trip the value through an extra `f64` multiply) and
/// does all arithmetic in `Decimal`. Division by 1,000,000 is an exact
/// base-10 rescale, so the result carries no representational error.
/// Returns `Decimal::ZERO` when the model has no catalog entry.
pub fn calculate_cost_decimal(model: &str, input_tokens: i64, output_tokens: i64) -> Decimal {
    let Some(pricing) = crate::llm_config::model_pricing_for_input_tokens(model, input_tokens)
    else {
        return Decimal::ZERO;
    };
    let gross = Decimal::from(input_tokens) * authored_rate_decimal(pricing.input_per_mtok)
        + Decimal::from(output_tokens) * authored_rate_decimal(pricing.output_per_mtok);
    gross / Decimal::from(1_000_000i64)
}

/// Calculate cost using catalog model pricing first, then provider catalog
/// economics when the model has no exact catalog entry. Returns 0.0 when
/// pricing is unknown (use `pricing_detail_for` to distinguish unknown).
pub fn calculate_cost_for_provider(
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> f64 {
    let Some(detail) = pricing_detail_for_usage(provider, model, input_tokens) else {
        return 0.0;
    };
    (input_tokens as f64 * detail.input_per_1k + output_tokens as f64 * detail.output_per_1k)
        / 1000.0
}

/// Per-call USD cost with cache accounting, preserving unknown pricing.
/// Budget arithmetic may explicitly choose a zero lower bound, but evidence
/// and receipts must use this projection so an unpriced call never masquerades
/// as free.
pub(crate) fn pricing_aware_call_cost_with_cache(
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
) -> Option<f64> {
    if provider.eq_ignore_ascii_case("mock") {
        return Some(0.0);
    }
    let detail = pricing_detail_for_usage(provider, model, input_tokens)?;
    Some(project_call_cost(
        &detail,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
    ))
}

/// Per-call USD cost for trace attribution, or `None` when the
/// (provider, model) pair has no catalog pricing. Unlike
/// [`calculate_cost_for_provider`] (which coerces unknown pricing to
/// `0.0` for budget arithmetic), this preserves the distinction so a
/// `cost_usd` span field can honestly report "unpriced" rather than a
/// misleading zero. Pricing resolution matches `calculate_cost_for_provider`:
/// catalog model rate first, then provider-level economics.
pub fn pricing_aware_call_cost(
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> Option<f64> {
    if provider.eq_ignore_ascii_case("mock") {
        return Some(0.0);
    }
    let detail = pricing_detail_for_usage(provider, model, input_tokens)?;
    Some(
        (input_tokens as f64 * detail.input_per_1k + output_tokens as f64 * detail.output_per_1k)
            / 1000.0,
    )
}

pub(crate) fn cache_hit_ratio(
    input_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
) -> f64 {
    let input_tokens = input_tokens.max(0);
    let cache_read_tokens = cache_read_tokens.max(0);
    let cache_write_tokens = cache_write_tokens.max(0);
    let reported_cache_tokens = cache_read_tokens.saturating_add(cache_write_tokens);
    let total_prompt_tokens = if reported_cache_tokens <= input_tokens {
        input_tokens
    } else {
        input_tokens.saturating_add(reported_cache_tokens)
    };
    if total_prompt_tokens == 0 {
        0.0
    } else {
        cache_read_tokens as f64 / total_prompt_tokens as f64
    }
}

pub(crate) fn cache_savings_usd_for_provider(
    provider: &str,
    model: &str,
    input_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
) -> f64 {
    let Some(detail) = pricing_detail_for_usage(provider, model, input_tokens) else {
        return 0.0;
    };
    let input_rate = detail.input_per_1k;
    let cache_read_rate = detail.cache_read_per_1k.unwrap_or(input_rate);
    let cache_write_rate = detail.cache_write_per_1k.unwrap_or(input_rate);
    let cache_read_savings =
        cache_read_tokens.max(0) as f64 * (input_rate - cache_read_rate) / 1000.0;
    let cache_write_savings =
        cache_write_tokens.max(0) as f64 * (input_rate - cache_write_rate) / 1000.0;
    cache_read_savings + cache_write_savings
}

fn accumulate_llm_usage(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cost: f64,
) -> Result<(), VmError> {
    // Always attribute usage to the active `@step` (if any), even when
    // the per-call cost is zero — token-only step budgets need the
    // count regardless of pricing.
    crate::step_runtime::record_step_llm_usage(model, input_tokens, output_tokens, cost)?;
    let total_tokens = input_tokens.max(0) as u64 + output_tokens.max(0) as u64;
    if total_tokens > 0 {
        LLM_ACCUMULATED_TOKENS.with(|acc| {
            let mut slot = acc.borrow_mut();
            *slot = slot.saturating_add(total_tokens);
        });
        LLM_TOKEN_BUDGET.with(|budget| {
            if let Some(max) = *budget.borrow() {
                let total = LLM_ACCUMULATED_TOKENS.with(|acc| *acc.borrow());
                if total > max {
                    return Err(categorized_error(
                        format!("LLM token budget exceeded: spent {total} of {max} tokens"),
                        ErrorCategory::BudgetExceeded,
                    ));
                }
            }
            Ok(())
        })?;
    }
    if cost == 0.0 {
        return Ok(());
    }
    LLM_ACCUMULATED_COST.with(|acc| {
        *acc.borrow_mut() += cost;
    });
    LLM_BUDGET.with(|budget| {
        if let Some(max) = *budget.borrow() {
            let total = LLM_ACCUMULATED_COST.with(|acc| *acc.borrow());
            if total > max {
                return Err(categorized_error(
                    format!("LLM budget exceeded: spent ${total:.4} of ${max:.4} budget"),
                    ErrorCategory::BudgetExceeded,
                ));
            }
        }
        Ok(())
    })
}

pub(crate) fn record_llm_usage(result: &crate::llm::api::LlmResult) -> Result<(), VmError> {
    let usage = result.usage();
    record_observed_session_usage(&usage);
    accumulate_llm_usage(
        &result.model,
        usage.input_tokens,
        usage.output_tokens,
        usage.cost_usd.unwrap_or(0.0),
    )
}

pub(crate) fn register_cost_builtins(vm: &mut Vm) {
    vm.register_builtin_def(&TIKTOKEN_COUNT_TOKENS_IMPL_DEF);
    vm.register_builtin_def(&TIKTOKEN_TOKENIZER_INFO_IMPL_DEF);
    vm.register_builtin_def(&TIKTOKEN_ENCODE_TOKENS_IMPL_DEF);
    vm.register_builtin_def(&TIKTOKEN_DECODE_TOKENS_IMPL_DEF);
    vm.register_builtin_def(&LLM_COST_IMPL_DEF);
    vm.register_builtin_def(&LLM_PRICING_BUILTIN_DEF);
    vm.register_builtin_def(&LLM_FORMAT_USD_BUILTIN_DEF);
    vm.register_builtin_def(&LLM_COMPARE_COSTS_BUILTIN_DEF);
    vm.register_builtin_def(&LLM_SESSION_COST_IMPL_DEF);
    vm.register_builtin_def(&LLM_BUDGET_IMPL_DEF);
    vm.register_builtin_def(&LLM_BUDGET_REMAINING_IMPL_DEF);
}

#[harn_builtin(exposure = "pure", effects = [], sig = "llm_cost(model: string, input_tokens: int, output_tokens: int) -> decimal", category = "llm.economics")]
fn llm_cost_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let model = args.first().map(VmValue::display).unwrap_or_default();
    let input_tokens = args.get(1).and_then(VmValue::as_int).unwrap_or(0);
    let output_tokens = args.get(2).and_then(VmValue::as_int).unwrap_or(0);
    Ok(VmValue::decimal(calculate_cost_decimal(
        &model,
        input_tokens,
        output_tokens,
    )))
}

#[harn_builtin(exposure = "privileged_wire", effects = ["state.observe@const=llm-cost-ledger"], sig = "__llm_session_cost() -> dict", category = "llm.economics")]
fn llm_session_cost_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (total_input, total_output, _duration, call_count) = super::trace::peek_trace_summary();
    let total_cost = LLM_ACCUMULATED_COST.with(|acc| *acc.borrow());
    let mut result = BTreeMap::new();
    result.insert("total_cost".to_string(), VmValue::Float(total_cost));
    result.insert("input_tokens".to_string(), VmValue::Int(total_input));
    result.insert("output_tokens".to_string(), VmValue::Int(total_output));
    result.insert("call_count".to_string(), VmValue::Int(call_count));
    Ok(VmValue::dict(result))
}

#[harn_builtin(exposure = "privileged_wire", effects = ["state.mutate@const=llm-cost-budget"], sig = "__llm_budget(max_cost: float | int) -> nil", category = "llm.economics")]
fn llm_budget_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let max_cost = match args.first() {
        Some(VmValue::Float(value)) => *value,
        Some(VmValue::Int(value)) => *value as f64,
        _ => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "llm_budget: requires a numeric argument",
            ))));
        }
    };
    set_llm_cost_budget(Some(max_cost));
    Ok(VmValue::Nil)
}

#[harn_builtin(exposure = "privileged_wire", effects = ["state.observe@const=llm-cost-budget"], sig = "__llm_budget_remaining() -> float?", category = "llm.economics")]
fn llm_budget_remaining_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let remaining = LLM_BUDGET.with(|budget| {
        budget.borrow().map(|max| {
            let spent = LLM_ACCUMULATED_COST.with(|acc| *acc.borrow());
            max - spent
        })
    });
    Ok(remaining.map(VmValue::Float).unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "tiktoken_count_tokens(text: string, model: string) -> int",
    category = "llm.budget"
)]
fn tiktoken_count_tokens_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(VmValue::display).unwrap_or_default();
    let model = args.get(1).map(VmValue::display).unwrap_or_default();
    if model.trim().is_empty() {
        return Err(VmError::Runtime(
            "tiktoken_count_tokens: model is required".to_string(),
        ));
    }
    let estimate = super::token_count::tiktoken_count_text(&text, &model)
        .map_err(|error| VmError::Runtime(format!("tiktoken_count_tokens: {error}")))?;
    Ok(VmValue::Int(estimate.tokens))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "tiktoken_tokenizer_info(model: string) -> dict",
    category = "llm.budget"
)]
fn tiktoken_tokenizer_info_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let model = args.first().map(VmValue::display).unwrap_or_default();
    Ok(tokenizer_info_to_vm_value(
        &model,
        super::token_count::tokenizer_info_for_model(&model),
    ))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "tiktoken_encode_tokens(text: string, model: string) -> list<{_type: \"llm_token\", id: int, tokenizer: string, bytes: list<int>, text: string?}>",
    category = "llm.tokenizer"
)]
fn tiktoken_encode_tokens_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = args.first().map(VmValue::display).unwrap_or_default();
    let model = args.get(1).map(VmValue::display).unwrap_or_default();
    if model.trim().is_empty() {
        return Err(VmError::Runtime(
            "tiktoken_encode_tokens: model is required".to_string(),
        ));
    }
    let tokens = super::token_count::tokenize_exact(&text, &model)
        .map_err(|error| VmError::Runtime(format!("tiktoken_encode_tokens: {error}")))?;
    Ok(VmValue::List(std::sync::Arc::new(
        tokens
            .into_iter()
            .map(|token| {
                let mut value = BTreeMap::new();
                value.put_str("_type", "llm_token");
                value.insert("id".to_string(), VmValue::Int(i64::from(token.id)));
                value.put_str("tokenizer", &token.tokenizer);
                value.insert(
                    "bytes".to_string(),
                    VmValue::List(std::sync::Arc::new(
                        token
                            .bytes
                            .iter()
                            .map(|byte| VmValue::Int(i64::from(*byte)))
                            .collect(),
                    )),
                );
                value.insert(
                    "text".to_string(),
                    String::from_utf8(token.bytes)
                        .map(|text| VmValue::String(arcstr::ArcStr::from(text)))
                        .unwrap_or(VmValue::Nil),
                );
                VmValue::dict(value)
            })
            .collect(),
    )))
}

#[harn_builtin(
    exposure = "pure",
    effects = [],
    sig = "tiktoken_decode_tokens(tokens: list<{_type: \"llm_token\", id: int, tokenizer: string, bytes: list<int>, text: string?}>) -> string",
    category = "llm.tokenizer"
)]
fn tiktoken_decode_tokens_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let Some(VmValue::List(tokens)) = args.first() else {
        return Err(VmError::Runtime(
            "tiktoken_decode_tokens: tokens must be a list of TokenRef records".to_string(),
        ));
    };
    if tokens.is_empty() {
        return Ok(VmValue::String(arcstr::ArcStr::from("")));
    }
    let mut identity: Option<String> = None;
    let mut ids = Vec::with_capacity(tokens.len());
    for (index, token) in tokens.iter().enumerate() {
        let VmValue::Dict(token) = token else {
            return Err(VmError::Runtime(format!(
                "tiktoken_decode_tokens: token at index {index} is not a TokenRef record"
            )));
        };
        let token_identity = token
            .get("tokenizer")
            .map(VmValue::display)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                VmError::Runtime(format!(
                    "tiktoken_decode_tokens: token at index {index} has no tokenizer identity"
                ))
            })?;
        if identity
            .as_ref()
            .is_some_and(|expected| expected != &token_identity)
        {
            return Err(VmError::Runtime(format!(
                "tiktoken_decode_tokens: token at index {index} uses `{token_identity}`, but the sequence uses `{}`",
                identity.as_deref().unwrap_or_default()
            )));
        }
        identity.get_or_insert(token_identity);
        let id = match token.get("id") {
            Some(VmValue::Int(id)) => u32::try_from(*id).map_err(|_| {
                VmError::Runtime(format!(
                    "tiktoken_decode_tokens: token at index {index} has an out-of-range id"
                ))
            })?,
            _ => {
                return Err(VmError::Runtime(format!(
                    "tiktoken_decode_tokens: token at index {index} has no integer id"
                )))
            }
        };
        ids.push(id);
    }
    let text = super::token_count::detokenize_exact(identity.as_deref().unwrap_or_default(), &ids)
        .map_err(|error| VmError::Runtime(format!("tiktoken_decode_tokens: {error}")))?;
    Ok(VmValue::String(arcstr::ArcStr::from(text)))
}

fn pricing_detail_to_vm_value(provider: &str, model: &str, detail: &PricingDetail) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.put_str("provider", provider);
    dict.put_str("model", model);
    dict.insert(
        "input_per_mtok".to_string(),
        VmValue::Float(detail.input_per_1k * 1000.0),
    );
    dict.insert(
        "output_per_mtok".to_string(),
        VmValue::Float(detail.output_per_1k * 1000.0),
    );
    dict.insert(
        "cache_read_per_mtok".to_string(),
        detail
            .cache_read_per_1k
            .map(|rate| VmValue::Float(rate * 1000.0))
            .unwrap_or(VmValue::Nil),
    );
    dict.insert(
        "cache_write_per_mtok".to_string(),
        detail
            .cache_write_per_1k
            .map(|rate| VmValue::Float(rate * 1000.0))
            .unwrap_or(VmValue::Nil),
    );
    dict.put_str("source", detail.source.as_str());
    VmValue::dict(dict)
}

fn resolve_pricing_args(args: &[VmValue]) -> (String, String) {
    if let Some(VmValue::Dict(dict)) = args.first() {
        let provider = dict
            .get("provider")
            .map(|value| value.display())
            .unwrap_or_default();
        let model = dict
            .get("model")
            .map(|value| value.display())
            .unwrap_or_default();
        if !provider.is_empty() && !model.is_empty() {
            return (provider, model);
        }
        if !model.is_empty() {
            let resolved = crate::llm_config::resolve_model_info(&model);
            return (resolved.provider, resolved.id);
        }
    }
    let first = args.first().map(|a| a.display()).unwrap_or_default();
    let second = args.get(1).map(|a| a.display()).unwrap_or_default();
    match (first.is_empty(), second.is_empty()) {
        (false, false) => (first, second),
        (false, true) => {
            let resolved = crate::llm_config::resolve_model_info(&first);
            (resolved.provider, resolved.id)
        }
        _ => (String::new(), String::new()),
    }
}

#[harn_builtin(exposure = "pure", effects = [], sig = "llm_pricing(model_or_dict: string | dict, model?: string) -> dict?", category = "llm.economics")]
fn llm_pricing_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (provider, model) = resolve_pricing_args(args);
    if model.trim().is_empty() {
        return Err(VmError::Runtime(
            "llm_pricing: model is required".to_string(),
        ));
    }
    Ok(pricing_detail_for(&provider, &model)
        .map(|detail| pricing_detail_to_vm_value(&provider, &model, &detail))
        .unwrap_or(VmValue::Nil))
}

#[harn_builtin(exposure = "pure", effects = [], sig = "llm_format_usd(amount: decimal | float | int, options?: dict) -> string", category = "llm.economics")]
fn llm_format_usd_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let amount = match args.first() {
        Some(VmValue::Float(value)) => *value,
        Some(VmValue::Int(value)) => *value as f64,
        // Accept exact `decimal` amounts (e.g. the result of `llm_cost`).
        // Formatting rounds to a fixed number of display decimals anyway, so
        // converting to `f64` for the digit layout is lossless at that
        // precision; the exact value never feeds back into money math here.
        Some(VmValue::Decimal(value)) => {
            use rust_decimal::prelude::ToPrimitive;
            value.to_f64().unwrap_or(0.0)
        }
        Some(VmValue::Nil) | None => 0.0,
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "llm_format_usd: amount must be a number (got {})",
                other.type_name(),
            )))
        }
    };
    let options = args.get(1).and_then(|v| v.as_dict());
    let explicit_precision = options
        .and_then(|opts| opts.get("precision"))
        .and_then(|value| match value {
            VmValue::Int(n) if *n >= 0 => Some(*n as usize),
            VmValue::Float(f) if f.is_finite() && *f >= 0.0 => Some(*f as usize),
            _ => None,
        });
    let sign_always = options
        .and_then(|opts| opts.get("sign"))
        .and_then(|value| match value {
            VmValue::Bool(b) => Some(*b),
            _ => None,
        })
        .unwrap_or(false);
    let formatted = format_usd_amount(amount, explicit_precision, sign_always);
    Ok(VmValue::String(arcstr::ArcStr::from(formatted)))
}

fn format_usd_amount(amount: f64, precision: Option<usize>, sign_always: bool) -> String {
    if !amount.is_finite() {
        return "$NaN".to_string();
    }
    let precision = precision.unwrap_or_else(|| {
        let abs = amount.abs();
        if abs == 0.0 || abs >= 100.0 {
            2
        } else if abs >= 1.0 {
            4
        } else {
            6
        }
    });
    let sign = if amount < 0.0 {
        "-"
    } else if sign_always {
        "+"
    } else {
        ""
    };
    // Defer rounding to the libc formatter so that values like 81.0 that
    // arrive as 80.999… don't split into "$80." + "1.0000".
    let rounded = format!("{:.*}", precision, amount.abs());
    #[expect(
        clippy::string_slice,
        reason = "idx comes from find('.') on the ASCII-formatted number"
    )]
    let (whole_str, frac_part) = match rounded.find('.') {
        Some(idx) => (&rounded[..idx], &rounded[idx + 1..]),
        None => (rounded.as_str(), ""),
    };
    let mut grouped = String::new();
    for (idx, ch) in whole_str.chars().enumerate() {
        if idx > 0 && (whole_str.len() - idx) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    if precision == 0 || frac_part.is_empty() {
        format!("{sign}${grouped}")
    } else {
        format!("{sign}${grouped}.{frac_part}")
    }
}

#[harn_builtin(exposure = "pure", effects = [], sig = "llm_compare_costs(candidates: list, options: dict) -> list", category = "llm.economics")]
fn llm_compare_costs_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let candidates = match args.first() {
        Some(VmValue::List(items)) => items.clone(),
        _ => {
            return Err(VmError::Runtime(
                "llm_compare_costs: candidates must be a list".to_string(),
            ))
        }
    };
    let opts = match args.get(1) {
        Some(VmValue::Dict(dict)) => dict.clone(),
        _ => {
            return Err(VmError::Runtime(
                "llm_compare_costs: options dict is required".to_string(),
            ))
        }
    };
    let input_tokens = opts
        .get("input_tokens")
        .and_then(|v| v.as_int())
        .unwrap_or(0)
        .max(0);
    let output_tokens = opts
        .get("output_tokens")
        .and_then(|v| v.as_int())
        .unwrap_or(0)
        .max(0);
    let cache_read_tokens = opts
        .get("cache_read_tokens")
        .and_then(|v| v.as_int())
        .unwrap_or(0)
        .max(0);
    let cache_write_tokens = opts
        .get("cache_write_tokens")
        .and_then(|v| v.as_int())
        .unwrap_or(0)
        .max(0);
    let calls = opts
        .get("calls")
        .and_then(|v| v.as_int())
        .unwrap_or(1)
        .max(1);

    let mut rows: Vec<(Option<f64>, VmValue)> = Vec::with_capacity(candidates.len());
    for candidate in candidates.iter() {
        let (provider, model) = match candidate {
            VmValue::Dict(dict) => {
                let provider = dict
                    .get("provider")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let model = dict.get("model").map(|v| v.display()).unwrap_or_default();
                if model.is_empty() {
                    return Err(VmError::Runtime(
                        "llm_compare_costs: each candidate dict must include `model`".to_string(),
                    ));
                }
                if provider.is_empty() {
                    let resolved = crate::llm_config::resolve_model_info(&model);
                    (resolved.provider, resolved.id)
                } else {
                    (provider, model)
                }
            }
            VmValue::String(s) => {
                let resolved = crate::llm_config::resolve_model_info(s);
                (resolved.provider, resolved.id)
            }
            _ => {
                return Err(VmError::Runtime(format!(
                    "llm_compare_costs: candidates must be strings or dicts (got {})",
                    candidate.type_name(),
                )))
            }
        };
        let detail = pricing_detail_for_usage(&provider, &model, input_tokens);
        let projection = detail.map(|d| {
            project_call_cost(
                &d,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
            ) * calls as f64
        });
        let mut row = BTreeMap::new();
        row.put_str("provider", provider.clone());
        row.put_str("model", model.clone());
        row.insert(
            "pricing".to_string(),
            detail
                .as_ref()
                .map(|d| pricing_detail_to_vm_value(&provider, &model, d))
                .unwrap_or(VmValue::Nil),
        );
        row.insert(
            "cost_usd".to_string(),
            projection.map(VmValue::Float).unwrap_or(VmValue::Nil),
        );
        row.insert("calls".to_string(), VmValue::Int(calls));
        row.insert("pricing_known".to_string(), VmValue::Bool(detail.is_some()));
        rows.push((projection, VmValue::dict(row)));
    }

    rows.sort_by(|left, right| match (left.0, right.0) {
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    Ok(VmValue::List(std::sync::Arc::new(
        rows.into_iter().map(|(_, value)| value).collect(),
    )))
}

pub(crate) fn project_call_cost(
    detail: &PricingDetail,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_write_tokens: i64,
) -> f64 {
    let cache_read_rate = detail.cache_read_per_1k.unwrap_or(detail.input_per_1k);
    let cache_write_rate = detail.cache_write_per_1k.unwrap_or(detail.input_per_1k);
    // Providers report cache tokens under two conventions. OpenAI folds cached
    // tokens into `input_tokens`, so cached counts must be subtracted to avoid
    // double-billing. Anthropic (and OpenRouter-Anthropic) report `input_tokens`
    // already excluding cache, with cache counts in separate fields, so the raw
    // input is the non-cached remainder. Normalize the same way `cache_hit_ratio`
    // does: if the cache total fits within input, treat cache as a subset;
    // otherwise treat input as already exclusive of cache.
    let cache_total = cache_read_tokens.saturating_add(cache_write_tokens);
    let billable_input = if cache_total <= input_tokens {
        input_tokens - cache_total
    } else {
        input_tokens
    }
    .max(0);
    (billable_input as f64 * detail.input_per_1k
        + output_tokens as f64 * detail.output_per_1k
        + cache_read_tokens as f64 * cache_read_rate
        + cache_write_tokens as f64 * cache_write_rate)
        / 1000.0
}

fn tokenizer_info_to_vm_value(model: &str, info: super::token_count::TokenizerInfo) -> VmValue {
    let mut result = BTreeMap::new();
    result.put_str("model", model);
    result.put_str("model_family", info.model_family);
    result.put_str("source", info.source.as_str());
    result.insert("exact".to_string(), VmValue::Bool(info.exact));
    result.insert(
        "known_model_family".to_string(),
        VmValue::Bool(info.known_model_family),
    );
    result.insert(
        "encoder".to_string(),
        info.encoder
            .map(|encoder| VmValue::String(arcstr::ArcStr::from(encoder)))
            .unwrap_or(VmValue::Nil),
    );
    result.insert(
        "identity".to_string(),
        if info.exact {
            super::token_count::exact_tokenizer_identity_for_model(model)
                .ok()
                .map(|identity| VmValue::String(arcstr::ArcStr::from(identity)))
                .unwrap_or(VmValue::Nil)
        } else {
            VmValue::Nil
        },
    );
    VmValue::dict(result)
}

#[cfg(test)]
#[path = "cost_tests.rs"]
mod tests;
