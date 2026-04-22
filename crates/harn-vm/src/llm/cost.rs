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
    model_pricing_per_million(model).map(|(input, output)| (input / 1000.0, output / 1000.0))
}

pub(crate) fn pricing_per_1k_for(provider: &str, model: &str) -> Option<(f64, f64)> {
    model_pricing_per_1k(model).or_else(|| {
        let (input, output, _) = crate::llm_config::provider_economics(provider);
        match (input, output) {
            (Some(input), Some(output)) => Some((input, output)),
            _ => None,
        }
    })
}

pub(crate) fn latency_p50_ms_for(provider: &str) -> Option<u64> {
    let (_, _, latency) = crate::llm_config::provider_economics(provider);
    latency
}

/// Calculate cost for a given model and token counts.
pub fn calculate_cost(model: &str, input_tokens: i64, output_tokens: i64) -> f64 {
    match model_pricing_per_million(model) {
        Some((input_rate, output_rate)) => {
            (input_tokens as f64 * input_rate + output_tokens as f64 * output_rate) / 1_000_000.0
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
