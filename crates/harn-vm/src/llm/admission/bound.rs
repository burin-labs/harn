//! Bounds use the full catalog context, never a request tokenizer estimate or
//! prior output/cache averages. Unsupported billing shapes are refused.
use rust_decimal::Decimal;

use super::{error, money, DenialKind};
use crate::llm::api::{LlmRequestPayload, LlmResult, PromptCacheTtl};
use crate::value::VmError;

pub(super) struct AttemptBound {
    catalog_id: String,
    input_limit: i64,
    output_limit: i64,
    input_rate: Decimal,
    output_rate: Decimal,
    retain_full_input: bool,
}

impl AttemptBound {
    pub(super) fn for_request(request: &LlmRequestPayload) -> Result<Self, VmError> {
        let anthropic = request.provider == "anthropic";
        if !(request.provider == "openai" || anthropic)
            || request.fast
            || request.vision
            || !request.provider_tools.is_empty()
            || request.provider_overrides.is_some()
            || request.previous_response_id.is_some()
            || request.background == Some(true)
            || request.compact == Some(true)
            || request.prediction.is_some()
            || !request.anthropic_beta_features.is_empty()
            || request
                .native_tools
                .as_ref()
                .is_some_and(|tools| tools.iter().any(|tool| !function_tool(tool, anthropic)))
            || !request
                .messages
                .iter()
                .all(|message| text_message(message, anthropic))
            || (anthropic
                && (request.prompt_cache_ttl == Some(PromptCacheTtl::OneHour)
                    || !request.messages.iter().all(ordinary_message_cache_control)
                    || request
                        .native_tools
                        .as_ref()
                        .is_some_and(|tools| !tools.iter().all(ordinary_cache_control))))
        {
            return Err(error(
                DenialKind::UnsupportedBillingShape,
                "conservative admission does not support this provider or billing shape",
            ));
        }
        let catalog_id =
            crate::llm_config::model_catalog_id_for_route(&request.provider, &request.model)
                .ok_or_else(|| {
                    error(
                        DenialKind::UnknownPricing,
                        "conservative admission requires an exact catalog route",
                    )
                })?;
        let row = crate::llm_config::model_catalog_entry(&catalog_id)
            .ok_or_else(|| error(DenialKind::UnknownPricing, "missing catalog model"))?;
        let input_limit = i64::try_from(
            row.context_window
                .max(row.runtime_context_window.unwrap_or(0)),
        )
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            error(
                DenialKind::UnsupportedBillingShape,
                "conservative admission requires a catalog context limit",
            )
        })?;
        let pricing = row.pricing.ok_or_else(|| {
            error(
                DenialKind::UnknownPricing,
                "conservative admission requires known model pricing",
            )
        })?;
        if anthropic && pricing.cache_write_per_mtok.is_none() {
            return Err(error(
                DenialKind::UnknownPricing,
                "conservative Anthropic admission requires a known cache-write rate",
            ));
        }
        // An expiring discount must not reduce a reservation. Until the rate
        // contract exposes a temporal upper bound, these rows are unsupported.
        if !pricing.promotions.is_empty() || request.max_tokens <= 0 {
            return Err(error(
                DenialKind::UnsupportedBillingShape,
                "conservative admission requires stable pricing and an explicit output limit",
            ));
        }
        let mut input_rate = Decimal::ZERO;
        let mut output_rate = Decimal::ZERO;
        // Inspect every whole-request band; do not assume multipliers increase.
        let thresholds =
            std::iter::once(0).chain(pricing.input_token_bands.iter().filter_map(|b| {
                i64::try_from(b.minimum_input_tokens)
                    .ok()
                    .filter(|n| *n <= input_limit)
            }));
        for threshold in thresholds {
            let band = pricing.for_input_tokens(threshold);
            for rate in [
                Some(band.input_per_mtok),
                band.cache_read_per_mtok,
                band.cache_write_per_mtok,
            ]
            .into_iter()
            .flatten()
            {
                input_rate = input_rate.max(money(rate)?);
            }
            output_rate = output_rate.max(money(band.output_per_mtok)?);
        }
        Ok(Self {
            catalog_id,
            input_limit,
            output_limit: request.max_tokens,
            input_rate,
            output_rate,
            retain_full_input: anthropic,
        })
    }

    fn cost(&self, input: i64, output: i64) -> Decimal {
        (Decimal::from(input) * self.input_rate + Decimal::from(output) * self.output_rate)
            / Decimal::from(1_000_000)
    }

    pub(super) fn total(&self) -> Decimal {
        self.cost(self.input_limit, self.output_limit)
    }

    /// Settle at the conservative rates, without claiming a billing receipt or
    /// applying cache discounts. Missing or inconsistent wire usage keeps the
    /// full reservation. Native OpenAI counters include reasoning output.
    pub(super) fn observed_upper(&self, result: &LlmResult) -> Option<Decimal> {
        let input = result.telemetry.server_prompt_tokens?;
        let output = result.telemetry.server_output_tokens?;
        let route = crate::llm_config::model_catalog_id_for_route(&result.provider, &result.model)?;
        if (result.provider != "openai" && result.provider != "anthropic")
            || route != self.catalog_id
            || result.served_fast
            || input < 0
            || output < 0
            || (!self.retain_full_input && input != result.input_tokens)
            || output != result.output_tokens
        {
            return None;
        }
        // Anthropic's wire input counter is fresh input, not the full prompt.
        // The result does not retain presence for both cache counters. Keep
        // the full input reservation instead of treating an omitted category
        // as known zero. Output-only release still has an authoritative bound.
        let input_upper = if self.retain_full_input {
            self.input_limit.max(result.input_tokens).max(input)
        } else {
            input
        };
        Some(self.cost(input_upper, output))
    }
}

fn text_message(message: &serde_json::Value, anthropic: bool) -> bool {
    if !message.as_object().is_some_and(|fields| {
        fields.keys().all(|key| {
            matches!(
                key.as_str(),
                "role" | "content" | "name" | "tool_calls" | "tool_call_id"
            ) || (anthropic && key == "cache_control")
        })
    }) {
        return false;
    }
    // Function arguments/results are text; image/audio/file payloads require a
    // separate pricing contract. Reject even unknown content part kinds.
    match message.get("content") {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::String(_)) => true,
        Some(serde_json::Value::Array(parts)) => parts.iter().all(|part| {
            matches!(
                part.get("type").and_then(|v| v.as_str()),
                Some("text" | "input_text" | "output_text")
            )
        }),
        _ => false,
    }
}

fn function_tool(tool: &serde_json::Value, anthropic: bool) -> bool {
    if anthropic {
        // The native Anthropic client-function schema has no `type` field.
        // Server tools carry their own typed billing contract and stay refused.
        tool.get("type").is_none()
            && tool.get("name").is_some_and(|value| value.is_string())
            && tool
                .get("input_schema")
                .is_some_and(|value| value.is_object())
    } else {
        tool.get("type").and_then(|value| value.as_str()) == Some("function")
    }
}

fn ordinary_message_cache_control(message: &serde_json::Value) -> bool {
    ordinary_cache_control(message)
        && message
            .get("content")
            .and_then(|value| value.as_array())
            .is_none_or(|parts| parts.iter().all(ordinary_cache_control))
}

fn ordinary_cache_control(value: &serde_json::Value) -> bool {
    // Only wire-level breakpoints affect billing. Function argument/schema
    // properties named `cache_control` are user data, not cache directives.
    value.get("cache_control").is_none_or(|control| {
        control.get("type").and_then(|value| value.as_str()) == Some("ephemeral")
            && control
                .get("ttl")
                .is_none_or(|ttl| ttl.as_str() == Some("5m"))
    })
}
