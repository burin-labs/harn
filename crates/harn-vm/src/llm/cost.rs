use crate::value::VmDictExt;
use rust_decimal::Decimal;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::str::FromStr;

use crate::value::{categorized_error, ErrorCategory, VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity, VmBuiltinMetadata};

thread_local! {
    static LLM_BUDGET: RefCell<Option<f64>> = const { RefCell::new(None) };
    static LLM_ACCUMULATED_COST: RefCell<f64> = const { RefCell::new(0.0) };
    static LLM_TOKEN_BUDGET: RefCell<Option<u64>> = const { RefCell::new(None) };
    static LLM_ACCUMULATED_TOKENS: RefCell<u64> = const { RefCell::new(0) };
}

/// Reset thread-local cost state. Call between test runs to avoid leaking.
pub(crate) fn reset_cost_state() {
    LLM_BUDGET.with(|b| *b.borrow_mut() = None);
    LLM_ACCUMULATED_COST.with(|a| *a.borrow_mut() = 0.0);
    LLM_TOKEN_BUDGET.with(|b| *b.borrow_mut() = None);
    LLM_ACCUMULATED_TOKENS.with(|a| *a.borrow_mut() = 0);
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
}

impl Drop for LlmBudgetGuard {
    fn drop(&mut self) {
        LLM_BUDGET.with(|b| *b.borrow_mut() = self.previous_budget);
        LLM_ACCUMULATED_COST.with(|a| *a.borrow_mut() = self.previous_accumulated);
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
    LLM_BUDGET.with(|b| *b.borrow_mut() = Some(max_cost_usd.max(0.0)));
    LLM_ACCUMULATED_COST.with(|a| *a.borrow_mut() = 0.0);
    LlmBudgetGuard {
        previous_budget,
        previous_accumulated,
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
/// each call ([`accumulate_cost_for_provider`]). A supervisor on the dispatch
/// thread can therefore tighten or loosen the ceiling mid-run and have the next
/// LLM call observe it — the basis for ACP `session/set_budget` re-arm
/// (burin-labs/burin-code#1561). Callers that want fresh per-scope accounting
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

#[derive(Clone, Debug, Default, PartialEq)]
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
    pub projected_output_tokens: i64,
    pub projected_cost_usd: f64,
    pub session_cost_usd: f64,
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

pub(crate) fn parse_budget_envelope(
    options: Option<&crate::value::DictMap>,
) -> Result<Option<LlmBudgetEnvelope>, VmError> {
    let Some(options) = options else {
        return Ok(None);
    };
    let mut envelope = LlmBudgetEnvelope::default();
    if let Some(value) = options.get("budget") {
        match value {
            VmValue::Nil => {}
            VmValue::Dict(fields) => parse_budget_fields(fields, &mut envelope)?,
            _ => {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    "budget: expected a dict {max_cost_usd?, total_budget_usd?, max_input_tokens?, max_output_tokens?}",
                ))));
            }
        }
    }
    parse_budget_fields(options, &mut envelope)?;
    Ok((!envelope.is_empty()).then_some(envelope))
}

fn estimate_json_tokens(value: &serde_json::Value, model: &str) -> i64 {
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

pub(crate) fn project_llm_call_cost(
    opts: &super::api::LlmCallOptions,
    session_cost_usd: f64,
) -> LlmBudgetProjection {
    let system_tokens = opts
        .system
        .as_deref()
        .map(|system| estimate_text_tokens_for_model(system, &opts.model))
        .unwrap_or(0);
    let message_tokens: i64 = opts
        .messages
        .iter()
        .map(|message| estimate_json_tokens(message, &opts.model))
        .sum();
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
    let projected_input_tokens = system_tokens
        .saturating_add(message_tokens)
        .saturating_add(tool_tokens);
    let projected_output_tokens = opts.max_tokens.max(0);
    let projected_cost_usd = calculate_cost_for_provider(
        &opts.provider,
        &opts.model,
        projected_input_tokens,
        projected_output_tokens,
    );
    LlmBudgetProjection {
        provider: opts.provider.clone(),
        model: opts.model.clone(),
        projected_input_tokens,
        projected_output_tokens,
        projected_cost_usd,
        session_cost_usd,
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
                    "projected session cost ${:.6}",
                    projection.session_cost_usd + projection.projected_cost_usd
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
    /// The model's accelerated-serving tier (`fast_mode.pricing`), used when
    /// the provider confirmed it served the request fast.
    CatalogFastMode,
    /// Provider-level catalog economics (`[llm.providers.<name>]`).
    ProviderEconomics,
}

impl PricingSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PricingSource::CatalogModel => "catalog_model",
            PricingSource::CatalogFastMode => "catalog_fast_mode",
            PricingSource::ProviderEconomics => "provider_economics",
        }
    }
}

/// Resolve full pricing detail for a (provider, model) pair. Prefers the
/// exact-id catalog entry, then falls back to provider-level economics.
/// Returns `None` for unknown pricing — callers must decide whether to
/// surface that explicitly or coerce to 0.0.
pub(crate) fn pricing_detail_for(provider: &str, model: &str) -> Option<PricingDetail> {
    if let Some(pricing) = crate::llm_config::model_pricing_per_mtok(model) {
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

pub(crate) fn pricing_per_1k_for(provider: &str, model: &str) -> Option<(f64, f64)> {
    pricing_detail_for(provider, model).map(|p| (p.input_per_1k, p.output_per_1k))
}

/// Resolve pricing for a (provider, model) pair, billing at the premium
/// accelerated-serving tier when `served_fast` is set and the catalog
/// declares `fast_mode.pricing`. Falls back to standard pricing when the
/// request was served at the standard tier (e.g. a capacity downgrade) or
/// the fast tier omits explicit rates.
pub(crate) fn pricing_detail_for_tier(
    provider: &str,
    model: &str,
    served_fast: bool,
) -> Option<PricingDetail> {
    if served_fast {
        if let Some(pricing) = crate::llm_config::model_fast_pricing_per_mtok(model) {
            return Some(PricingDetail {
                input_per_1k: pricing.input_per_mtok / 1000.0,
                output_per_1k: pricing.output_per_mtok / 1000.0,
                cache_read_per_1k: pricing.cache_read_per_mtok.map(|rate| rate / 1000.0),
                cache_write_per_1k: pricing.cache_write_per_mtok.map(|rate| rate / 1000.0),
                source: PricingSource::CatalogFastMode,
            });
        }
    }
    pricing_detail_for(provider, model)
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
    let Some(pricing) = crate::llm_config::model_pricing_per_mtok(model) else {
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
    let Some(detail) = pricing_detail_for(provider, model) else {
        return 0.0;
    };
    (input_tokens as f64 * detail.input_per_1k + output_tokens as f64 * detail.output_per_1k)
        / 1000.0
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
    let detail = pricing_detail_for(provider, model)?;
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
    cache_read_tokens: i64,
    cache_write_tokens: i64,
) -> f64 {
    let Some(detail) = pricing_detail_for(provider, model) else {
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

pub(crate) fn accumulate_cost_for_provider(
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    served_fast: bool,
) -> Result<(), VmError> {
    let cost = pricing_detail_for_tier(provider, model, served_fast)
        .map(|detail| {
            (input_tokens as f64 * detail.input_per_1k
                + output_tokens as f64 * detail.output_per_1k)
                / 1000.0
        })
        .unwrap_or(0.0);
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

pub(crate) fn record_llm_usage_for_provider(
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    served_fast: bool,
) -> Result<(), VmError> {
    accumulate_cost_for_provider(provider, model, input_tokens, output_tokens, served_fast)
}

pub(crate) fn register_cost_builtins(vm: &mut Vm) {
    vm.register_builtin("llm_cost", |args, _out| {
        let model = args.first().map(|a| a.display()).unwrap_or_default();
        let input_tokens = args.get(1).and_then(|a| a.as_int()).unwrap_or(0);
        let output_tokens = args.get(2).and_then(|a| a.as_int()).unwrap_or(0);
        // Return the cost as an exact `decimal` (not a binary `float`): the
        // value names money, summing it should not drift, and the catalog
        // rates are recovered exactly. Callers that want a formatted string
        // pass the result to `llm_format_usd`, which accepts decimals.
        let cost = calculate_cost_decimal(&model, input_tokens, output_tokens);
        Ok(VmValue::decimal(cost))
    });

    vm.register_builtin_with_metadata(
        VmBuiltinMetadata::sync_static("llm_pricing")
            .signature_static("llm_pricing(model_or_dict, model?)")
            .arity(VmBuiltinArity::Range { min: 1, max: 2 })
            .category_static("llm.economics")
            .doc_static(
                "Return catalog pricing for a model: \
                {input_per_mtok, output_per_mtok, cache_read_per_mtok, cache_write_per_mtok, \
                provider, model, source} or nil if the model has no priced entry.",
            ),
        llm_pricing_builtin,
    );

    vm.register_builtin_with_metadata(
        VmBuiltinMetadata::sync_static("llm_format_usd")
            .signature_static("llm_format_usd(amount, options?)")
            .arity(VmBuiltinArity::Range { min: 1, max: 2 })
            .category_static("llm.economics")
            .doc_static(
                "Format a USD amount as a string. Default precision auto-scales: 6 decimals \
                under $1, 4 decimals under $100, 2 decimals otherwise; pass {precision: N} to override.",
            ),
        llm_format_usd_builtin,
    );

    vm.register_builtin_with_metadata(
        VmBuiltinMetadata::sync_static("llm_compare_costs")
            .signature_static("llm_compare_costs(candidates, opts)")
            .arity(VmBuiltinArity::Exact(2))
            .category_static("llm.economics")
            .doc_static(
                "Project a per-call cost across a list of {provider?, model} candidates given \
                {input_tokens, output_tokens, cache_read_tokens?, cache_write_tokens?, calls?}. \
                Returns a list sorted ascending by projected cost (unknown pricing trails).",
            ),
        llm_compare_costs_builtin,
    );

    vm.register_builtin("llm_session_cost", |_args, _out| {
        let (total_input, total_output, _duration, call_count) = super::trace::peek_trace_summary();
        let total_cost = LLM_ACCUMULATED_COST.with(|acc| *acc.borrow());
        let mut result = BTreeMap::new();
        result.insert("total_cost".to_string(), VmValue::Float(total_cost));
        result.insert("input_tokens".to_string(), VmValue::Int(total_input));
        result.insert("output_tokens".to_string(), VmValue::Int(total_output));
        result.insert("call_count".to_string(), VmValue::Int(call_count));
        Ok(VmValue::dict(result))
    });

    vm.register_builtin("llm_budget", |args, _out| {
        let max_cost = match args.first() {
            Some(VmValue::Float(f)) => *f,
            Some(VmValue::Int(n)) => *n as f64,
            _ => {
                return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                    "llm_budget: requires a numeric argument",
                ))));
            }
        };
        set_llm_cost_budget(Some(max_cost));
        Ok(VmValue::Nil)
    });

    vm.register_builtin("llm_budget_remaining", |_args, _out| {
        let remaining = LLM_BUDGET.with(|budget| {
            budget.borrow().map(|max| {
                let spent = LLM_ACCUMULATED_COST.with(|acc| *acc.borrow());
                max - spent
            })
        });
        match remaining {
            Some(r) => Ok(VmValue::Float(r)),
            None => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin_with_metadata(
        VmBuiltinMetadata::sync_static("tiktoken_count_tokens")
            .signature_static("tiktoken_count_tokens(text, model)")
            .arity(VmBuiltinArity::Exact(2))
            .category_static("llm.budget")
            .doc_static("Count text tokens with the tiktoken encoder selected for a model."),
        |args, _out| {
            let text = args.first().map(|arg| arg.display()).unwrap_or_default();
            let model = args.get(1).map(|arg| arg.display()).unwrap_or_default();
            if model.trim().is_empty() {
                return Err(VmError::Runtime(
                    "tiktoken_count_tokens: model is required".to_string(),
                ));
            }
            let estimate = super::token_count::tiktoken_count_text(&text, &model)
                .map_err(|error| VmError::Runtime(format!("tiktoken_count_tokens: {error}")))?;
            Ok(VmValue::Int(estimate.tokens))
        },
    );

    vm.register_builtin_with_metadata(
        VmBuiltinMetadata::sync_static("tiktoken_tokenizer_info")
            .signature_static("tiktoken_tokenizer_info(model)")
            .arity(VmBuiltinArity::Exact(1))
            .category_static("llm.budget")
            .doc_static("Return the tiktoken encoder metadata used for a model token count."),
        |args, _out| {
            let model = args.first().map(|arg| arg.display()).unwrap_or_default();
            Ok(tokenizer_info_to_vm_value(
                &model,
                super::token_count::tokenizer_info_for_model(&model),
            ))
        },
    );
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
        let detail = pricing_detail_for(&provider, &model);
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
    let billable_input = (input_tokens - cache_read_tokens - cache_write_tokens).max(0);
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
    VmValue::dict(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculate_cost_uses_catalog_model_pricing() {
        let _guard = crate::llm::env_guard();
        let mut overlay = crate::llm_config::ProvidersConfig::default();
        overlay.models.insert(
            "gpt-4o-mini".to_string(),
            crate::llm_config::ModelDef {
                name: "Test GPT-4o Mini".to_string(),
                provider: "openai".to_string(),
                context_window: 128_000,
                logical_model: None,
                equivalence_group: None,
                served_variant: None,
                wire_model: None,
                api_dialect: None,
                rate_limits: None,
                performance: None,
                architecture: None,
                local_memory: None,
                runtime_context_window: None,
                stream_timeout: None,
                capabilities: Vec::new(),
                pricing: Some(crate::llm_config::ModelPricing {
                    input_per_mtok: 10.0,
                    output_per_mtok: 20.0,
                    cache_read_per_mtok: None,
                    cache_write_per_mtok: None,
                }),
                deprecated: false,
                deprecation_note: None,
                superseded_by: None,
                fast_mode: None,
                quality_tags: Vec::new(),
                availability: crate::llm_config::ModelAvailability::default(),
                tier: None,
                open_weight: None,
                strengths: Vec::new(),
                benchmarks: std::collections::BTreeMap::new(),
                family: None,
                lineage: None,
                complementary_with: Vec::new(),
                avoid_as_reviewer_for: Vec::new(),
            },
        );
        crate::llm_config::set_user_overrides(Some(overlay));

        // 1000*10 + 1000*20 = 30000; /1e6 = 0.03, exactly.
        assert_eq!(
            calculate_cost_decimal("gpt-4o-mini", 1000, 1000),
            Decimal::from_str("0.03").unwrap()
        );

        crate::llm_config::clear_user_overrides();
    }

    #[test]
    fn calculate_cost_is_zero_for_unknown_model() {
        let _guard = crate::llm::env_guard();
        crate::llm_config::clear_user_overrides();
        assert_eq!(
            calculate_cost_decimal("definitely-unpriced-model", 1_000, 1_000),
            Decimal::ZERO
        );
    }

    #[test]
    fn authored_rate_decimal_recovers_the_written_literal_not_float_noise() {
        // Catalog rates are short literals parsed from TOML into f64; many
        // (0.15, 0.8, 0.08) are not exactly representable in binary. The
        // recovery must reconstruct the *authored* decimal, never the f64's
        // binary-rounding tail that `from_f64_retain` would expose.
        for (raw, written) in [
            (0.15_f64, "0.15"),
            (0.8, "0.8"),
            (0.08, "0.08"),
            (4.0, "4"),
            (0.0, "0"),
            (3.75, "3.75"),
        ] {
            let recovered = authored_rate_decimal(raw);
            assert_eq!(
                recovered,
                Decimal::from_str(written).unwrap(),
                "rate {raw} should recover as {written}"
            );
        }
        // Guard the whole point of the helper: it must NOT equal the lossy
        // `from_f64_retain` decimal for an inexact literal.
        assert_ne!(
            authored_rate_decimal(0.1),
            Decimal::from_f64_retain(0.1).unwrap()
        );
    }

    #[test]
    fn calculate_cost_decimal_is_exact_for_inexact_catalog_rates() {
        let _guard = crate::llm::env_guard();
        let mut overlay = crate::llm_config::ProvidersConfig::default();
        overlay.models.insert(
            "gpt-4o-mini".to_string(),
            crate::llm_config::ModelDef {
                name: "Test GPT-4o Mini".to_string(),
                provider: "openai".to_string(),
                context_window: 128_000,
                logical_model: None,
                equivalence_group: None,
                served_variant: None,
                wire_model: None,
                api_dialect: None,
                rate_limits: None,
                performance: None,
                architecture: None,
                local_memory: None,
                runtime_context_window: None,
                stream_timeout: None,
                capabilities: Vec::new(),
                // Inexact-in-binary rates, like the real catalog.
                pricing: Some(crate::llm_config::ModelPricing {
                    input_per_mtok: 0.15,
                    output_per_mtok: 0.60,
                    cache_read_per_mtok: None,
                    cache_write_per_mtok: None,
                }),
                deprecated: false,
                deprecation_note: None,
                superseded_by: None,
                fast_mode: None,
                quality_tags: Vec::new(),
                availability: crate::llm_config::ModelAvailability::default(),
                tier: None,
                open_weight: None,
                strengths: Vec::new(),
                benchmarks: std::collections::BTreeMap::new(),
                family: None,
                lineage: None,
                complementary_with: Vec::new(),
                avoid_as_reviewer_for: Vec::new(),
            },
        );
        crate::llm_config::set_user_overrides(Some(overlay));

        // 1000 * 0.15 + 500 * 0.60 = 150 + 300 = 450; /1e6 = 0.00045 exactly.
        assert_eq!(
            calculate_cost_decimal("gpt-4o-mini", 1000, 500),
            Decimal::from_str("0.00045").unwrap()
        );

        crate::llm_config::clear_user_overrides();
    }

    #[test]
    fn calculate_cost_for_provider_falls_back_to_provider_economics() {
        let _guard = crate::llm::env_guard();
        crate::llm_config::clear_user_overrides();
        let cost =
            calculate_cost_for_provider("openai", "some-bespoke-openai-deployment", 1_000, 1_000);
        let (input_per_1k, output_per_1k, _) = crate::llm_config::provider_economics("openai");
        let expected =
            (1_000.0 * input_per_1k.unwrap() + 1_000.0 * output_per_1k.unwrap()) / 1_000.0;
        assert!(
            (cost - expected).abs() < 1e-9,
            "cost={cost}, expected={expected}"
        );
    }

    #[test]
    fn pricing_detail_reports_source() {
        let _guard = crate::llm::env_guard();
        crate::llm_config::clear_user_overrides();
        let exact = pricing_detail_for("anthropic", "claude-sonnet-4-20250514").unwrap();
        assert_eq!(exact.source, PricingSource::CatalogModel);
        assert!(exact.cache_read_per_1k.is_some());

        let provider_only = pricing_detail_for("openai", "some-bespoke-openai-deployment").unwrap();
        assert_eq!(provider_only.source, PricingSource::ProviderEconomics);
        assert!(provider_only.cache_read_per_1k.is_none());

        assert!(pricing_detail_for("local", "no-such-local-model").is_some()); // local has 0/0
        assert!(pricing_detail_for("nonexistent_provider", "ghost-model").is_none());
    }

    #[test]
    fn pricing_aware_call_cost_distinguishes_unpriced_from_zero() {
        let _guard = crate::llm::env_guard();
        crate::llm_config::clear_user_overrides();

        // Known catalog model: Some(cost) matching the priced arithmetic.
        let priced = pricing_aware_call_cost("anthropic", "claude-sonnet-4-20250514", 1_000, 1_000);
        let expected =
            calculate_cost_for_provider("anthropic", "claude-sonnet-4-20250514", 1_000, 1_000);
        assert!(priced.is_some());
        assert!((priced.unwrap() - expected).abs() < 1e-9);

        // Genuinely unpriced (provider not in the catalog economics table):
        // None, not a misleading 0.0. `calculate_cost_for_provider` coerces
        // the same case to 0.0, which is exactly the ambiguity this helper
        // exists to remove.
        assert_eq!(
            pricing_aware_call_cost("nonexistent_provider", "ghost-model", 1_000, 1_000),
            None
        );
        assert_eq!(
            calculate_cost_for_provider("nonexistent_provider", "ghost-model", 1_000, 1_000),
            0.0
        );
    }

    #[test]
    fn format_usd_amount_auto_precision_and_grouping() {
        assert_eq!(format_usd_amount(0.000_045, None, false), "$0.000045");
        assert_eq!(format_usd_amount(1.234_5, None, false), "$1.2345");
        assert_eq!(format_usd_amount(1234.5, None, false), "$1,234.50");
        assert_eq!(format_usd_amount(-1234.5, None, false), "-$1,234.50");
        assert_eq!(format_usd_amount(1234.5, None, true), "+$1,234.50");
        assert_eq!(format_usd_amount(0.123_456_789, Some(2), false), "$0.12");
        assert_eq!(format_usd_amount(1.0, Some(0), false), "$1");
    }

    #[test]
    fn format_usd_handles_fractional_carry_into_whole() {
        // 0.00027 * 300_000 produces 80.999… in IEEE-754; the formatter
        // must round-then-render rather than splitting the rounded fraction
        // back into a separate component (regression: "$80.1.0000").
        let amount = 0.000_27_f64 * 300_000.0;
        assert!((amount - 81.0).abs() < 1e-6);
        assert_eq!(format_usd_amount(amount, None, false), "$81.0000");
    }

    #[test]
    fn fast_tier_bills_premium_pricing_when_served_fast() {
        let _guard = crate::llm::env_guard();
        crate::llm_config::clear_user_overrides();

        // Opus 4.8 fast mode is 2x standard ($5/$25 -> $10/$50 per MTok).
        let standard = pricing_detail_for_tier("anthropic", "claude-opus-4-8", false).unwrap();
        let fast = pricing_detail_for_tier("anthropic", "claude-opus-4-8", true).unwrap();
        assert_eq!(standard.source, PricingSource::CatalogModel);
        assert_eq!(fast.source, PricingSource::CatalogFastMode);
        assert!((fast.input_per_1k - 2.0 * standard.input_per_1k).abs() < 1e-9);
        assert!((fast.output_per_1k - 2.0 * standard.output_per_1k).abs() < 1e-9);

        // A model with no fast tier ignores the flag and bills standard.
        let no_fast =
            pricing_detail_for_tier("anthropic", "claude-sonnet-4-20250514", true).unwrap();
        assert_eq!(no_fast.source, PricingSource::CatalogModel);
    }

    #[test]
    fn project_call_cost_excludes_cached_input_from_full_rate() {
        let detail = pricing_detail_for("anthropic", "claude-sonnet-4-20250514").unwrap();
        let with_cache = project_call_cost(&detail, 10_000, 500, 8_000, 0);
        let no_cache = project_call_cost(&detail, 10_000, 500, 0, 0);
        assert!(with_cache < no_cache);
    }

    #[test]
    fn cache_savings_uses_catalog_cache_pricing() {
        let _guard = crate::llm::env_guard();
        crate::llm_config::clear_user_overrides();

        let savings =
            cache_savings_usd_for_provider("anthropic", "claude-sonnet-4-20250514", 1000, 0);
        assert!((savings - 0.0027).abs() < 0.0000001);

        let write_delta =
            cache_savings_usd_for_provider("anthropic", "claude-sonnet-4-20250514", 0, 1000);
        assert!((write_delta + 0.00075).abs() < 0.0000001);

        crate::llm_config::clear_user_overrides();
    }

    #[test]
    fn cache_hit_ratio_handles_subset_and_separate_anthropic_counts() {
        assert!((cache_hit_ratio(1000, 250, 0) - 0.25).abs() < f64::EPSILON);
        assert!((cache_hit_ratio(100, 900, 0) - 0.9).abs() < f64::EPSILON);
        assert_eq!(cache_hit_ratio(0, 0, 0), 0.0);
    }

    #[test]
    fn token_budget_guard_restores_prior_state_on_drop() {
        let _guard_outer = crate::llm::env_guard();
        reset_cost_state();

        let outer = install_llm_token_budget(100);
        assert_eq!(peek_total_tokens(), 0);
        // Simulate accumulation by writing the thread-local directly.
        LLM_ACCUMULATED_TOKENS.with(|a| *a.borrow_mut() = 50);

        // Nested guard wipes accumulation and installs a tighter cap.
        {
            let _inner = install_llm_token_budget(10);
            assert_eq!(peek_total_tokens(), 0);
            LLM_ACCUMULATED_TOKENS.with(|a| *a.borrow_mut() = 5);
        }

        // Outer scope restored on inner drop.
        assert_eq!(peek_total_tokens(), 50);
        drop(outer);
        assert_eq!(peek_total_tokens(), 0);

        reset_cost_state();
    }

    #[test]
    fn set_budget_rearms_in_place_without_resetting_accumulation() {
        let _guard_outer = crate::llm::env_guard();
        reset_cost_state();

        // Install a $1.00 cap and spend $0.60 against it.
        let _budget = install_llm_cost_budget(1.0);
        LLM_ACCUMULATED_COST.with(|a| *a.borrow_mut() = 0.60);

        // Tighten the cap below current spend: the next preflight must trip,
        // and the already-accumulated total must be preserved (not reset).
        set_llm_cost_budget(Some(0.50));
        assert!((peek_total_cost() - 0.60).abs() < f64::EPSILON);
        LLM_BUDGET.with(|b| assert_eq!(*b.borrow(), Some(0.50)));

        // Loosen the cap: spend stays, ceiling rises, room reopens.
        set_llm_cost_budget(Some(2.0));
        assert!((peek_total_cost() - 0.60).abs() < f64::EPSILON);
        LLM_BUDGET.with(|b| assert_eq!(*b.borrow(), Some(2.0)));

        // Clear the cap entirely.
        set_llm_cost_budget(None);
        LLM_BUDGET.with(|b| assert_eq!(*b.borrow(), None));

        // Negative ceilings clamp to zero (a hard stop), matching `install_*`.
        set_llm_cost_budget(Some(-5.0));
        LLM_BUDGET.with(|b| assert_eq!(*b.borrow(), Some(0.0)));

        reset_cost_state();
    }

    #[test]
    fn set_token_budget_rearms_in_place_without_resetting_accumulation() {
        let _guard_outer = crate::llm::env_guard();
        reset_cost_state();

        let _budget = install_llm_token_budget(100);
        LLM_ACCUMULATED_TOKENS.with(|a| *a.borrow_mut() = 60);

        set_llm_token_budget(Some(50));
        assert_eq!(peek_total_tokens(), 60);
        LLM_TOKEN_BUDGET.with(|b| assert_eq!(*b.borrow(), Some(50)));

        set_llm_token_budget(None);
        assert_eq!(peek_total_tokens(), 60);
        LLM_TOKEN_BUDGET.with(|b| assert_eq!(*b.borrow(), None));

        reset_cost_state();
    }

    #[test]
    fn token_budget_raises_categorized_error_when_exhausted() {
        let _guard_outer = crate::llm::env_guard();
        reset_cost_state();
        let _budget = install_llm_token_budget(10);

        // First call within budget — admits.
        let first =
            accumulate_cost_for_provider("anthropic", "claude-sonnet-4-20250514", 5, 0, false);
        assert!(first.is_ok());

        // Second call pushes over — raises BudgetExceeded.
        let second =
            accumulate_cost_for_provider("anthropic", "claude-sonnet-4-20250514", 8, 0, false);
        match second {
            Err(VmError::CategorizedError { category, message }) => {
                assert_eq!(category, ErrorCategory::BudgetExceeded);
                assert!(message.contains("token budget"), "got: {message}");
            }
            other => panic!("expected BudgetExceeded, got {other:?}"),
        }

        reset_cost_state();
    }
}
