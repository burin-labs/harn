//! Provider-reported non-text billing units. Missing counters stay absent.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) fn deserialize_optional<'de, D>(
    deserializer: D,
) -> Result<Option<Box<BillingUsage>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    BillingUsage::deserialize(deserializer).map(BillingUsage::present)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BillingUsage {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invalid_modality_counts: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hosted_tool_calls: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_audio_input_tokens: Option<u64>,
}

pub(crate) struct BillingSettlement {
    pub total_usd: f64,
    pub platform_fee_estimate_usd: f64,
    pub hosted_tool_unpriced: Vec<String>,
    pub modality_unpriced: Vec<String>,
    pub monthly_allowance_unapplied: Vec<String>,
}

impl BillingSettlement {
    pub fn has_unpriced_units(&self) -> bool {
        !self.hosted_tool_unpriced.is_empty() || !self.modality_unpriced.is_empty()
    }
}

impl BillingUsage {
    pub(crate) fn settle(
        &self,
        detail: &crate::llm::cost::PricingDetail,
        token_cost: f64,
    ) -> BillingSettlement {
        let multiplier = 1.0 + detail.platform_fee_percent / 100.0;
        let mut subtotal = token_cost / multiplier;
        let mut unpriced = Vec::new();
        let mut allowances = Vec::new();
        let mut modality_unpriced = Vec::new();
        for (tool, count) in &self.hosted_tool_calls {
            match detail.hosted_tool_fees.get(tool) {
                Some(fee) => {
                    subtotal += *count as f64 * fee.per_1k_calls / 1000.0;
                    if fee.free_per_month.is_some() {
                        allowances.push(tool.clone());
                    }
                }
                None if *count > 0 => unpriced.push(tool.clone()),
                None => {}
            }
        }
        let rates = detail.modality_rates.as_ref();
        let cached = self.cached_audio_input_tokens.unwrap_or(0);
        if self.invalid_modality_counts {
            modality_unpriced.push("invalid_modality_counts".into());
        }
        for (name, count, rate, text_rate) in [
            (
                "audio_input",
                self.audio_input_tokens.unwrap_or(0).saturating_sub(cached),
                rates.and_then(|r| r.audio_input_per_mtok),
                detail.input_per_1k * 1000.0,
            ),
            (
                "audio_output",
                self.audio_output_tokens.unwrap_or(0),
                rates.and_then(|r| r.audio_output_per_mtok),
                detail.output_per_1k * 1000.0,
            ),
            (
                "cached_audio_input",
                cached,
                rates.and_then(|r| r.cached_audio_input_per_mtok),
                detail.cache_read_per_1k.unwrap_or(detail.input_per_1k) * 1000.0,
            ),
        ] {
            if count > 0 && !self.invalid_modality_counts {
                match rate {
                    Some(rate) => subtotal += count as f64 * (rate - text_rate) / 1_000_000.0,
                    None => {
                        // These tokens are audio, so their text-token charge
                        // is not part of the known priced portion.
                        subtotal -= count as f64 * text_rate / 1_000_000.0;
                        modality_unpriced.push(name.into());
                    }
                }
            }
        }
        // Replacing every text token can leave a tiny negative float residue.
        let subtotal = subtotal.max(0.0);
        let platform_fee = subtotal * detail.platform_fee_percent / 100.0;
        BillingSettlement {
            total_usd: subtotal + platform_fee,
            platform_fee_estimate_usd: platform_fee,
            hosted_tool_unpriced: unpriced,
            modality_unpriced,
            monthly_allowance_unapplied: allowances,
        }
    }
    pub(crate) fn from_openai(response: &Value) -> Option<Box<Self>> {
        let usage = &response["usage"];
        let input = usage
            .get("input_tokens_details")
            .or_else(|| usage.get("prompt_tokens_details"));
        let output = usage
            .get("output_tokens_details")
            .or_else(|| usage.get("completion_tokens_details"));
        let mut facts = Self {
            audio_input_tokens: input.and_then(|v| v["audio_tokens"].as_u64()),
            audio_output_tokens: output.and_then(|v| v["audio_tokens"].as_u64()),
            cached_audio_input_tokens: input
                .and_then(|v| v["cached_tokens_details"]["audio_tokens"].as_u64()),
            ..Self::default()
        };
        let input_total = usage
            .get("input_tokens")
            .or_else(|| usage.get("prompt_tokens"))
            .and_then(Value::as_u64);
        let output_total = usage
            .get("output_tokens")
            .or_else(|| usage.get("completion_tokens"))
            .and_then(Value::as_u64);
        facts.invalid_modality_counts = facts
            .audio_input_tokens
            .is_some_and(|count| input_total.is_none_or(|total| count > total))
            || facts
                .audio_output_tokens
                .is_some_and(|count| output_total.is_none_or(|total| count > total))
            || facts.cached_audio_input_tokens.is_some_and(|count| {
                facts.audio_input_tokens.is_none_or(|total| count > total)
                    || input
                        .and_then(|v| v["cached_tokens"].as_u64())
                        .is_none_or(|total| count > total)
            });
        for item in response["output"].as_array().into_iter().flatten() {
            let tool = match item["type"].as_str() {
                Some("web_search_call") => "web_search",
                Some("file_search_call") => "file_search",
                _ => continue,
            };
            *facts.hosted_tool_calls.entry(tool.into()).or_default() += 1;
        }
        facts.present()
    }

    pub(crate) fn from_anthropic(usage: &Value) -> Option<Box<Self>> {
        let mut facts = Self::default();
        if let Some(count) = usage["server_tool_use"]["web_search_requests"].as_u64() {
            facts.hosted_tool_calls.insert("web_search".into(), count);
        }
        facts.present()
    }

    pub(crate) fn from_gemini(response: &Value) -> Option<Box<Self>> {
        let queries: std::collections::BTreeSet<&str> = response["candidates"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|candidate| {
                candidate["groundingMetadata"]["webSearchQueries"]
                    .as_array()
                    .into_iter()
                    .flatten()
            })
            .filter_map(Value::as_str)
            .filter(|query| !query.is_empty())
            .collect();
        if queries.is_empty() {
            return None;
        }
        Some(Box::new(Self {
            hosted_tool_calls: BTreeMap::from([("google_search".into(), queries.len() as u64)]),
            ..Self::default()
        }))
    }

    fn present(self) -> Option<Box<Self>> {
        (self != Self::default()).then(|| Box::new(self))
    }

    pub(crate) fn add(&mut self, other: &Self) {
        self.invalid_modality_counts |= other.invalid_modality_counts;
        for (tool, count) in &other.hosted_tool_calls {
            let entry = self.hosted_tool_calls.entry(tool.clone()).or_default();
            *entry = entry.saturating_add(*count);
        }
        for (target, value) in [
            (&mut self.audio_input_tokens, other.audio_input_tokens),
            (&mut self.audio_output_tokens, other.audio_output_tokens),
            (
                &mut self.cached_audio_input_tokens,
                other.cached_audio_input_tokens,
            ),
        ] {
            if let Some(value) = value {
                *target = Some(target.unwrap_or(0).saturating_add(value));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_replaces_text_token_rates_before_platform_fee() {
        let _guard = crate::llm::env_guard();
        let mut detail = crate::llm::cost::pricing_detail_for(
            "openai",
            "gpt-5.6-luna",
            crate::llm::cost::settlement_now(),
        )
        .unwrap();
        detail.modality_rates = Some(crate::llm_config::ModalityRates {
            audio_input_per_mtok: Some(32.0),
            audio_output_per_mtok: Some(64.0),
            cached_audio_input_per_mtok: Some(0.4),
        });
        detail.platform_fee_percent = 5.5;
        let units = BillingUsage {
            audio_input_tokens: Some(1000),
            audio_output_tokens: Some(100),
            cached_audio_input_tokens: Some(200),
            ..BillingUsage::default()
        };
        let token_cost = crate::llm::cost::project_call_cost(&detail, 1000, 100, 200, 0, None);
        let settled = units.settle(&detail, token_cost);
        let expected = (800.0 * 32.0 + 200.0 * 0.4 + 100.0 * 64.0) / 1_000_000.0;
        assert!((settled.total_usd - expected * 1.055).abs() < 1e-12);
        assert!((settled.platform_fee_estimate_usd - expected * 0.055).abs() < 1e-12);
        assert!(!settled.has_unpriced_units());
    }
}
