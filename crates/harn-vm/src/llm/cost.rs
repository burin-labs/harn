use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

thread_local! {
    static LLM_BUDGET: RefCell<Option<f64>> = const { RefCell::new(None) };
    static LLM_ACCUMULATED_COST: RefCell<f64> = const { RefCell::new(0.0) };
}

/// Reset thread-local cost state. Call between test runs to avoid leaking.
pub(crate) fn reset_cost_state() {
    LLM_BUDGET.with(|b| *b.borrow_mut() = None);
    LLM_ACCUMULATED_COST.with(|a| *a.borrow_mut() = 0.0);
}

pub fn peek_total_cost() -> f64 {
    LLM_ACCUMULATED_COST.with(|acc| *acc.borrow())
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
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "budget.{key}: expected a non-negative number"
            )))));
        }
    };
    if !value.is_finite() || value < 0.0 {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "budget.{key}: expected a non-negative finite number"
        )))));
    }
    Ok(value)
}

fn integer_value(value: &VmValue, key: &str) -> Result<i64, VmError> {
    let value = match value {
        VmValue::Int(n) => *n,
        VmValue::Float(f) if f.is_finite() && f.fract() == 0.0 => *f as i64,
        _ => {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "budget.{key}: expected a non-negative integer"
            )))));
        }
    };
    if value < 0 {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "budget.{key}: expected a non-negative integer"
        )))));
    }
    Ok(value)
}

fn parse_budget_fields(
    fields: &BTreeMap<String, VmValue>,
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
    options: Option<&BTreeMap<String, VmValue>>,
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
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "budget: expected a dict {max_cost_usd?, total_budget_usd?, max_input_tokens?, max_output_tokens?}",
                ))));
            }
        }
    }
    parse_budget_fields(options, &mut envelope)?;
    Ok((!envelope.is_empty()).then_some(envelope))
}

fn estimate_json_tokens(value: &serde_json::Value) -> i64 {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => 1,
        serde_json::Value::String(s) => estimate_text_tokens(s),
        serde_json::Value::Array(items) => items.iter().map(estimate_json_tokens).sum(),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(key, value)| estimate_text_tokens(key) + estimate_json_tokens(value))
            .sum(),
    }
}

pub(crate) fn estimate_text_tokens(text: &str) -> i64 {
    if text.is_empty() {
        0
    } else {
        ((text.len() as f64) / 4.0).ceil() as i64
    }
}

pub(crate) fn project_llm_call_cost(
    opts: &super::api::LlmCallOptions,
    session_cost_usd: f64,
) -> LlmBudgetProjection {
    let system_tokens = opts
        .system
        .as_deref()
        .map(estimate_text_tokens)
        .unwrap_or(0);
    let message_tokens: i64 = opts.messages.iter().map(estimate_json_tokens).sum();
    let tool_tokens: i64 = opts
        .native_tools
        .as_ref()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| estimate_text_tokens(&serde_json::to_string(tool).unwrap_or_default()))
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
    dict.insert(
        "category".to_string(),
        VmValue::String(Rc::from("budget_exceeded")),
    );
    dict.insert("kind".to_string(), VmValue::String(Rc::from("terminal")));
    dict.insert(
        "reason".to_string(),
        VmValue::String(Rc::from("budget_exceeded")),
    );
    dict.insert(
        "limit".to_string(),
        VmValue::String(Rc::from(limit_kind.as_str())),
    );
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
        "provider".to_string(),
        VmValue::String(Rc::from(projection.provider.clone())),
    );
    dict.insert(
        "model".to_string(),
        VmValue::String(Rc::from(projection.model.clone())),
    );
    dict.insert(
        "message".to_string(),
        VmValue::String(Rc::from(format!(
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
        ))),
    );
    VmError::Thrown(VmValue::Dict(Rc::new(dict)))
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

/// Pricing per million tokens (input, output) in USD, as of early 2026.
fn model_pricing_per_million(model: &str) -> Option<(f64, f64)> {
    match model {
        // Anthropic
        m if m.contains("claude-3-5-haiku") || m.contains("claude-haiku-4") => Some((0.80, 4.00)),
        m if m.contains("claude-3-5-sonnet") || m.contains("claude-sonnet-4") => {
            Some((3.00, 15.00))
        }
        m if m.contains("claude-3-opus") || m.contains("claude-opus-4") => Some((15.00, 75.00)),
        // OpenAI
        m if m.contains("gpt-4o-mini") => Some((0.15, 0.60)),
        m if m.contains("gpt-4o") => Some((2.50, 10.00)),
        m if m.contains("gpt-4-turbo") => Some((10.00, 30.00)),
        m if m.contains("o1-mini") => Some((3.00, 12.00)),
        m if m.contains("o1") || m.contains("o3") => Some((15.00, 60.00)),
        // Meta / open source (typical hosted pricing)
        m if m.contains("llama") && m.contains("70b") => Some((0.59, 0.79)),
        m if m.contains("llama") && m.contains("8b") => Some((0.05, 0.08)),
        // Mistral
        m if m.contains("mistral-large") => Some((2.00, 6.00)),
        m if m.contains("mistral-small") => Some((0.20, 0.60)),
        // Google Gemini
        m if m.contains("gemini-2") && m.contains("flash") => Some((0.10, 0.40)),
        m if m.contains("gemini-2") && m.contains("pro") => Some((1.25, 5.00)),
        _ => None,
    }
}

/// Pricing per 1k tokens (input, output) in USD.
pub(crate) fn model_pricing_per_1k(model: &str) -> Option<(f64, f64)> {
    crate::llm_config::model_pricing_per_mtok(model)
        .map(|pricing| {
            (
                pricing.input_per_mtok / 1000.0,
                pricing.output_per_mtok / 1000.0,
            )
        })
        .or_else(|| {
            model_pricing_per_million(model)
                .map(|(input, output)| (input / 1000.0, output / 1000.0))
        })
}

pub(crate) fn pricing_per_1k_for(provider: &str, model: &str) -> Option<(f64, f64)> {
    model_pricing_per_1k(model).or_else(|| crate::llm_config::pricing_per_1k_for(provider, model))
}

fn model_cache_pricing_per_1k(model: &str) -> Option<(f64, Option<f64>, Option<f64>)> {
    crate::llm_config::model_pricing_per_mtok(model).map(|pricing| {
        (
            pricing.input_per_mtok / 1000.0,
            pricing.cache_read_per_mtok.map(|rate| rate / 1000.0),
            pricing.cache_write_per_mtok.map(|rate| rate / 1000.0),
        )
    })
}

fn cache_pricing_per_1k_for(
    provider: &str,
    model: &str,
) -> Option<(f64, Option<f64>, Option<f64>)> {
    model_cache_pricing_per_1k(model).or_else(|| {
        crate::llm_config::pricing_per_1k_for(provider, model)
            .map(|(input_rate, _output_rate)| (input_rate, None, None))
    })
}

pub(crate) fn latency_p50_ms_for(provider: &str) -> Option<u64> {
    let (_, _, latency) = crate::llm_config::provider_economics(provider);
    latency
}

/// Calculate cost for a given model and token counts.
pub fn calculate_cost(model: &str, input_tokens: i64, output_tokens: i64) -> f64 {
    match model_pricing_per_1k(model) {
        Some((input_rate, output_rate)) => {
            (input_tokens as f64 * input_rate + output_tokens as f64 * output_rate) / 1000.0
        }
        None => 0.0,
    }
}

/// Calculate cost using model-specific pricing first, then provider catalog
/// economics when the model is not in the static table.
pub fn calculate_cost_for_provider(
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> f64 {
    match pricing_per_1k_for(provider, model) {
        Some((input_rate, output_rate)) => {
            (input_tokens as f64 * input_rate + output_tokens as f64 * output_rate) / 1000.0
        }
        None => 0.0,
    }
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
    let Some((input_rate, cache_read_rate, cache_write_rate)) =
        cache_pricing_per_1k_for(provider, model)
    else {
        return 0.0;
    };
    let cache_read_savings = cache_read_tokens.max(0) as f64
        * (input_rate - cache_read_rate.unwrap_or(input_rate))
        / 1000.0;
    let cache_write_savings = cache_write_tokens.max(0) as f64
        * (input_rate - cache_write_rate.unwrap_or(input_rate))
        / 1000.0;
    cache_read_savings + cache_write_savings
}

pub(crate) fn accumulate_cost_for_provider(
    provider: &str,
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> Result<(), VmError> {
    let cost = calculate_cost_for_provider(provider, model, input_tokens, output_tokens);
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
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "LLM budget exceeded: spent ${:.4} of ${:.4} budget",
                    total, max
                )))));
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
) -> Result<(), VmError> {
    accumulate_cost_for_provider(provider, model, input_tokens, output_tokens)
}

pub(crate) fn register_cost_builtins(vm: &mut Vm) {
    vm.register_builtin("llm_cost", |args, _out| {
        let model = args.first().map(|a| a.display()).unwrap_or_default();
        let input_tokens = args.get(1).and_then(|a| a.as_int()).unwrap_or(0);
        let output_tokens = args.get(2).and_then(|a| a.as_int()).unwrap_or(0);
        let cost = calculate_cost(&model, input_tokens, output_tokens);
        Ok(VmValue::Float(cost))
    });

    vm.register_builtin("llm_session_cost", |_args, _out| {
        let (total_input, total_output, _duration, call_count) = super::trace::peek_trace_summary();
        let total_cost = LLM_ACCUMULATED_COST.with(|acc| *acc.borrow());
        let mut result = BTreeMap::new();
        result.insert("total_cost".to_string(), VmValue::Float(total_cost));
        result.insert("input_tokens".to_string(), VmValue::Int(total_input));
        result.insert("output_tokens".to_string(), VmValue::Int(total_output));
        result.insert("call_count".to_string(), VmValue::Int(call_count));
        Ok(VmValue::Dict(Rc::new(result)))
    });

    vm.register_builtin("llm_budget", |args, _out| {
        let max_cost = match args.first() {
            Some(VmValue::Float(f)) => *f,
            Some(VmValue::Int(n)) => *n as f64,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "llm_budget: requires a numeric argument",
                ))));
            }
        };
        LLM_BUDGET.with(|budget| {
            *budget.borrow_mut() = Some(max_cost);
        });
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculate_cost_uses_catalog_pricing_before_static_fallback() {
        let _guard = crate::llm::env_lock().lock().unwrap();
        let mut overlay = crate::llm_config::ProvidersConfig::default();
        overlay.models.insert(
            "gpt-4o-mini".to_string(),
            crate::llm_config::ModelDef {
                name: "Test GPT-4o Mini".to_string(),
                provider: "openai".to_string(),
                context_window: 128_000,
                stream_timeout: None,
                capabilities: Vec::new(),
                pricing: Some(crate::llm_config::ModelPricing {
                    input_per_mtok: 10.0,
                    output_per_mtok: 20.0,
                    cache_read_per_mtok: None,
                    cache_write_per_mtok: None,
                }),
            },
        );
        crate::llm_config::set_user_overrides(Some(overlay));

        let cost = calculate_cost("gpt-4o-mini", 1000, 1000);
        assert!((cost - 0.03).abs() < f64::EPSILON);

        crate::llm_config::clear_user_overrides();
    }

    #[test]
    fn cache_savings_uses_catalog_cache_pricing() {
        let _guard = crate::llm::env_lock().lock().unwrap();
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
}
