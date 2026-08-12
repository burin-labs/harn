//! Canonical LLM usage accounting and its public projections.
//!
//! Provider adapters own wire parsing, but once a call has produced token and
//! cache counts every consumer must read this ledger. VM envelopes,
//! transcripts, traces, metrics, and provider probes must not independently
//! recompute cost or cache semantics.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::value::{VmDictExt, VmValue};

use super::api::{LlmResult, ProviderAttempts};

/// The normalized accounting facts for one completed provider call.
///
/// This is the sole owner of derived cost/cache facts. It deliberately keeps
/// provider/model identity out of the public usage object: those remain route
/// metadata on the enclosing result and transcript event.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsageAccountingStatus {
    Reported,
    #[default]
    Unknown,
}

impl UsageAccountingStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LlmUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cost_usd: Option<f64>,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_supported: bool,
    pub cache_hit_ratio: Option<f64>,
    pub cache_savings_usd: f64,
    pub cache_hit: bool,
    pub served_fast: bool,
    #[serde(default)]
    pub accounting_status: UsageAccountingStatus,
}

impl LlmUsage {
    pub(crate) fn from_result(result: &LlmResult) -> Self {
        let usage_known = result.input_tokens > 0
            || result.output_tokens > 0
            || result.telemetry.server_prompt_tokens.is_some()
            || result.telemetry.server_output_tokens.is_some();
        let authoritative_cost = super::managed_supply::authoritative_cost_usd(result);
        let cost_usd = authoritative_cost.or_else(|| {
            if !usage_known {
                return None;
            }
            super::cost::pricing_detail_for_tier(
                &result.provider,
                &result.model,
                result.served_fast,
                result.input_tokens,
            )
            .map(|detail| {
                super::cost::project_call_cost(
                    &detail,
                    result.input_tokens,
                    result.output_tokens,
                    result.cache_read_tokens,
                    result.cache_write_tokens,
                )
            })
        });
        let cache_hit_ratio = result.cache_supported.then(|| {
            super::cost::cache_hit_ratio(
                result.input_tokens,
                result.cache_read_tokens,
                result.cache_write_tokens,
            )
        });
        Self {
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            cost_usd,
            cache_read_tokens: result.cache_read_tokens,
            cache_write_tokens: result.cache_write_tokens,
            cache_supported: result.cache_supported,
            cache_hit_ratio,
            cache_savings_usd: super::cost::cache_savings_usd_for_provider(
                &result.provider,
                &result.model,
                result.input_tokens,
                result.cache_read_tokens,
                result.cache_write_tokens,
            ),
            cache_hit: result.cache_read_tokens > 0,
            served_fast: result.served_fast,
            accounting_status: if usage_known || authoritative_cost.is_some() {
                UsageAccountingStatus::Reported
            } else {
                UsageAccountingStatus::Unknown
            },
        }
    }

    fn from_probe_counts(
        provider: &str,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Self {
        Self {
            input_tokens,
            output_tokens,
            cost_usd: super::cost::pricing_aware_call_cost(
                provider,
                model,
                input_tokens,
                output_tokens,
            ),
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_supported: false,
            cache_hit_ratio: None,
            cache_savings_usd: 0.0,
            cache_hit: false,
            served_fast: false,
            accounting_status: UsageAccountingStatus::Reported,
        }
    }

    /// Project the stable Harn `usage` envelope. Retry accounting is supplied
    /// by the observed-call boundary and stays nested under this one owner.
    pub(crate) fn to_vm_dict(&self, attempts: &ProviderAttempts) -> crate::value::DictMap {
        let mut usage = crate::value::DictMap::new();
        usage.insert(
            crate::value::intern_key("input_tokens"),
            VmValue::Int(self.input_tokens),
        );
        usage.insert(
            crate::value::intern_key("output_tokens"),
            VmValue::Int(self.output_tokens),
        );
        usage.insert(
            crate::value::intern_key("cost_usd"),
            self.cost_usd.map_or(VmValue::Nil, VmValue::Float),
        );
        usage.insert(
            crate::value::intern_key("cache_read_tokens"),
            VmValue::Int(self.cache_read_tokens),
        );
        usage.insert(
            crate::value::intern_key("cache_write_tokens"),
            VmValue::Int(self.cache_write_tokens),
        );
        usage.insert(
            crate::value::intern_key("cache_supported"),
            VmValue::Bool(self.cache_supported),
        );
        usage.insert(
            crate::value::intern_key("cache_hit_ratio"),
            self.cache_hit_ratio.map_or(VmValue::Nil, VmValue::Float),
        );
        if self.cache_supported {
            usage.insert(crate::value::intern_key("cache_visibility"), VmValue::Nil);
        } else {
            usage.put_str("cache_visibility", "unsupported");
        }
        usage.insert(
            crate::value::intern_key("cache_savings_usd"),
            VmValue::Float(self.cache_savings_usd),
        );
        usage.insert(
            crate::value::intern_key("provider_attempts"),
            VmValue::dict(provider_attempts_vm_dict(attempts)),
        );
        usage.insert(
            crate::value::intern_key("served_fast"),
            VmValue::Bool(self.served_fast),
        );
        usage.put_str("accounting_status", self.accounting_status.as_str());
        usage
    }

    /// Mechanically add the canonical accounting fields to the flat provider
    /// response event retained for CLI/backward compatibility.
    pub(crate) fn project_onto_event(&self, event: &mut serde_json::Value) {
        let fields = event
            .as_object_mut()
            .expect("usage projection target must be a JSON object");
        fields.insert("input_tokens".to_string(), self.input_tokens.into());
        fields.insert("output_tokens".to_string(), self.output_tokens.into());
        fields.insert(
            "cost_usd".to_string(),
            self.cost_usd.map_or(Value::Null, serde_json::Value::from),
        );
        fields.insert(
            "cache_read_tokens".to_string(),
            self.cache_read_tokens.into(),
        );
        fields.insert(
            "cache_write_tokens".to_string(),
            self.cache_write_tokens.into(),
        );
        fields.insert("cache_supported".to_string(), self.cache_supported.into());
        fields.insert(
            "cache_hit_ratio".to_string(),
            self.cache_hit_ratio
                .map_or(Value::Null, serde_json::Value::from),
        );
        fields.insert(
            "cache_visibility".to_string(),
            if self.cache_supported {
                Value::Null
            } else {
                Value::String("unsupported".to_string())
            },
        );
        fields.insert(
            "cache_savings_usd".to_string(),
            self.cache_savings_usd.into(),
        );
        fields.insert("cache_hit".to_string(), self.cache_hit.into());
        fields.insert("served_fast".to_string(), self.served_fast.into());
        fields.insert(
            "accounting_status".to_string(),
            self.accounting_status.as_str().into(),
        );
    }

    pub(crate) fn empty_vm_dict() -> crate::value::DictMap {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: None,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_supported: true,
            cache_hit_ratio: Some(0.0),
            cache_savings_usd: 0.0,
            cache_hit: false,
            served_fast: false,
            accounting_status: UsageAccountingStatus::Unknown,
        }
        .to_vm_dict(&ProviderAttempts::default())
    }

    /// Lower the ledger to canonical tracing metadata while keeping route
    /// identity on the enclosing call.
    pub(crate) fn metadata_pairs(
        &self,
        provider: &str,
        model: &str,
    ) -> Vec<(&'static str, serde_json::Value)> {
        use crate::tracing::meta;

        let mut pairs = vec![
            (meta::MODEL, serde_json::json!(model)),
            (meta::PROVIDER, serde_json::json!(provider)),
            (meta::INPUT_TOKENS, serde_json::json!(self.input_tokens)),
            (meta::OUTPUT_TOKENS, serde_json::json!(self.output_tokens)),
            (
                meta::CACHE_READ_TOKENS,
                serde_json::json!(self.cache_read_tokens),
            ),
            (
                meta::CACHE_WRITE_TOKENS,
                serde_json::json!(self.cache_write_tokens),
            ),
        ];
        if let Some(cost) = self.cost_usd {
            pairs.push((meta::COST_USD, serde_json::json!(cost)));
        }
        pairs
    }
}

fn provider_attempts_vm_dict(attempts: &ProviderAttempts) -> crate::value::DictMap {
    let mut fields = crate::value::DictMap::new();
    fields.insert(
        crate::value::intern_key("total"),
        VmValue::Int(i64::from(attempts.total)),
    );
    fields.insert(
        crate::value::intern_key("retries"),
        VmValue::Int(i64::from(attempts.retries())),
    );
    fields.insert(
        crate::value::intern_key("rate_limited"),
        VmValue::Int(i64::from(attempts.rate_limited)),
    );
    fields.insert(
        crate::value::intern_key("empty_completion"),
        VmValue::Int(i64::from(attempts.empty_completion)),
    );
    fields.insert(
        crate::value::intern_key("other"),
        VmValue::Int(i64::from(attempts.other)),
    );
    fields
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolProbeUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl ToolProbeUsage {
    fn from_totals(provider: &str, model: &str, totals: UsageTotals) -> Self {
        if let Some((input_tokens, output_tokens)) = totals.input_tokens.zip(totals.output_tokens) {
            let usage = LlmUsage::from_probe_counts(provider, model, input_tokens, output_tokens);
            return Self {
                input_tokens: Some(usage.input_tokens),
                output_tokens: Some(usage.output_tokens),
                cost_usd: usage.cost_usd,
            };
        }
        Self {
            input_tokens: totals.input_tokens,
            output_tokens: totals.output_tokens,
            cost_usd: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct UsageTotals {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
}

impl UsageTotals {
    fn has_any(self) -> bool {
        self.input_tokens.is_some() || self.output_tokens.is_some()
    }

    fn add_input(&mut self, value: i64) {
        self.input_tokens = Some(self.input_tokens.unwrap_or(0).saturating_add(value.max(0)));
    }

    fn add_output(&mut self, value: i64) {
        self.output_tokens = Some(self.output_tokens.unwrap_or(0).saturating_add(value.max(0)));
    }
}

pub(crate) fn extract_probe_usage(
    provider: &str,
    model: &str,
    response: &Value,
) -> Option<ToolProbeUsage> {
    let totals = usage_totals_from_response(response)?;
    Some(ToolProbeUsage::from_totals(provider, model, totals))
}

fn usage_totals_from_response(response: &Value) -> Option<UsageTotals> {
    let root_totals = usage_totals_from_envelope(response);
    if root_totals.has_any() {
        return Some(root_totals);
    }
    let frame_totals = last_stream_frame_usage(response);
    frame_totals.has_any().then_some(frame_totals)
}

fn last_stream_frame_usage(response: &Value) -> UsageTotals {
    let mut final_totals = UsageTotals::default();
    let Some(frames) = response.get("frames").and_then(Value::as_array) else {
        return final_totals;
    };
    for frame in frames {
        let frame_totals = usage_totals_from_envelope(frame);
        if frame_totals.has_any() {
            final_totals = frame_totals;
        }
    }
    final_totals
}

fn usage_totals_from_envelope(envelope: &Value) -> UsageTotals {
    let mut totals = UsageTotals::default();
    accumulate_usage_object(envelope.get("usage"), &mut totals);
    accumulate_usage_object(envelope.pointer("/message/usage"), &mut totals);
    accumulate_usage_object(envelope.get("usageMetadata"), &mut totals);
    accumulate_usage_object(envelope.pointer("/message/usageMetadata"), &mut totals);
    totals
}

fn accumulate_usage_object(usage: Option<&Value>, totals: &mut UsageTotals) {
    let Some(usage) = usage else {
        return;
    };
    if let Some(value) = first_i64_field(
        usage,
        &[
            "input_tokens",
            "prompt_tokens",
            "promptTokenCount",
            "prompt_token_count",
            "inputTokens",
        ],
    ) {
        totals.add_input(value);
    }

    let output_tokens = first_i64_field(
        usage,
        &[
            "output_tokens",
            "completion_tokens",
            "candidatesTokenCount",
            "completion_token_count",
            "outputTokenCount",
            "outputTokens",
        ],
    );
    let thoughts_tokens = first_i64_field(usage, &["thoughtsTokenCount", "thought_tokens"]);
    match (output_tokens, thoughts_tokens) {
        (Some(output), Some(thoughts)) => totals.add_output(output.saturating_add(thoughts)),
        (Some(output), None) => totals.add_output(output),
        (None, Some(thoughts)) => totals.add_output(thoughts),
        (None, None) => {}
    }
}

fn first_i64_field(value: &Value, names: &[&str]) -> Option<i64> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_i64))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::extract_probe_usage;
    use crate::llm::api::{LlmResult, ProviderAttempts, ProviderTelemetry};
    use crate::value::VmValue;

    fn accounted_result() -> LlmResult {
        LlmResult {
            text: "ok".to_string(),
            tool_calls: Vec::new(),
            text_projection: None,
            raw_tool_calls: Vec::new(),
            input_tokens: 1_000,
            output_tokens: 100,
            cache_read_tokens: 800,
            cache_write_tokens: 25,
            cache_supported: true,
            model: "claude-sonnet-4-20250514".to_string(),
            provider: "anthropic".to_string(),
            thinking: None,
            thinking_summary: None,
            stop_reason: Some("end_turn".to_string()),
            served_fast: false,
            blocks: Vec::new(),
            logprobs: Vec::new(),
            telemetry: ProviderTelemetry::default(),
            attempts: ProviderAttempts {
                total: 3,
                rate_limited: 1,
                empty_completion: 1,
                other: 0,
            },
        }
    }

    #[test]
    fn one_ledger_projects_matching_vm_event_and_trace_accounting() {
        let result = accounted_result();
        let usage = result.usage();
        let vm_usage =
            crate::llm::vm_value_to_json(&VmValue::Dict(usage.to_vm_dict(&result.attempts).into()));
        let mut event = json!({});
        usage.project_onto_event(&mut event);
        let trace = usage
            .metadata_pairs(&result.provider, &result.model)
            .into_iter()
            .collect::<std::collections::BTreeMap<_, _>>();

        for field in [
            "input_tokens",
            "output_tokens",
            "cost_usd",
            "cache_read_tokens",
            "cache_write_tokens",
            "cache_hit_ratio",
            "cache_savings_usd",
            "served_fast",
        ] {
            assert_eq!(
                vm_usage.get(field),
                event.get(field),
                "{field} drifted between canonical projections"
            );
        }
        assert_eq!(
            trace[crate::tracing::meta::INPUT_TOKENS],
            event["input_tokens"]
        );
        assert_eq!(
            trace[crate::tracing::meta::OUTPUT_TOKENS],
            event["output_tokens"]
        );
        assert_eq!(trace[crate::tracing::meta::COST_USD], event["cost_usd"]);
        assert_eq!(vm_usage["provider_attempts"]["retries"], json!(2));
    }

    #[test]
    fn missing_stream_usage_stays_unknown_instead_of_becoming_free() {
        let mut result = accounted_result();
        result.provider = "fireworks".to_string();
        result.model = "accounts/fireworks/models/minimax-m3".to_string();
        result.input_tokens = 0;
        result.output_tokens = 0;
        result.telemetry = ProviderTelemetry::from_openai_usage(
            &serde_json::json!({}),
            Some("chatcmpl-without-usage"),
        );

        let usage = result.usage();
        let vm_usage =
            crate::llm::vm_value_to_json(&VmValue::Dict(usage.to_vm_dict(&result.attempts).into()));

        assert_eq!(usage.cost_usd, None);
        assert_eq!(vm_usage["accounting_status"], "unknown");
        assert_eq!(vm_usage["cost_usd"], serde_json::Value::Null);
    }

    #[test]
    fn pre_accounting_status_record_replays_as_unknown() {
        let mut recorded = serde_json::to_value(accounted_result().usage()).expect("serialize");
        recorded
            .as_object_mut()
            .expect("usage object")
            .remove("accounting_status");

        let replayed: super::LlmUsage = serde_json::from_value(recorded).expect("old recording");

        assert_eq!(
            replayed.accounting_status,
            super::UsageAccountingStatus::Unknown
        );
    }

    #[test]
    fn public_usage_projections_do_not_recompute_accounting() {
        let projection_sources = [
            (
                "transcript",
                include_str!("agent_observe/transcript_observability.rs"),
            ),
            (
                "structured envelope",
                include_str!("structured_envelope.rs"),
            ),
            ("trace", include_str!("trace.rs")),
            ("agent result", include_str!("agent_config.rs")),
        ];
        for (name, source) in projection_sources {
            for forbidden in [
                "priced_cost_usd(",
                "cache_hit_ratio(",
                "cache_savings_usd_for_provider(",
                "struct LlmCallUsage",
            ] {
                assert!(
                    !source.contains(forbidden),
                    "{name} rebuilt canonical usage via {forbidden}"
                );
            }
        }
    }

    #[test]
    fn extracts_openai_responses_usage() {
        let response = json!({
            "usage": {
                "input_tokens": 11,
                "output_tokens": 7
            }
        });

        let usage = extract_probe_usage("unknown", "unknown", &response).expect("usage");

        assert_eq!(usage.input_tokens, Some(11));
        assert_eq!(usage.output_tokens, Some(7));
        assert_eq!(usage.cost_usd, None);
    }

    #[test]
    fn extracts_gemini_usage_metadata_with_thoughts() {
        let response = json!({
            "usageMetadata": {
                "promptTokenCount": 3,
                "candidatesTokenCount": 4,
                "thoughtsTokenCount": 9
            }
        });

        let usage = extract_probe_usage("gemini", "gemini-2.5-pro", &response).expect("usage");

        assert_eq!(usage.input_tokens, Some(3));
        assert_eq!(usage.output_tokens, Some(13));
    }

    #[test]
    fn extracts_vertex_usage_metadata_from_message_wrapper() {
        let response = json!({
            "message": {
                "usageMetadata": {
                    "promptTokenCount": 5,
                    "candidatesTokenCount": 8
                }
            }
        });

        let usage = extract_probe_usage("vertex", "gemini-2.5-flash", &response).expect("usage");

        assert_eq!(usage.input_tokens, Some(5));
        assert_eq!(usage.output_tokens, Some(8));
    }

    #[test]
    fn extracts_bedrock_usage_tokens() {
        let response = json!({
            "usage": {
                "inputTokens": 17,
                "outputTokens": 23
            }
        });

        let usage = extract_probe_usage("bedrock", "claude-sonnet-5", &response).expect("usage");

        assert_eq!(usage.input_tokens, Some(17));
        assert_eq!(usage.output_tokens, Some(23));
    }

    #[test]
    fn uses_final_stream_usage_without_double_counting_prior_frames() {
        let response = json!({
            "frames": [
                {
                    "usage": {
                        "prompt_tokens": 1,
                        "completion_tokens": 1
                    }
                },
                {
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 2
                    }
                }
            ]
        });

        let usage = extract_probe_usage("unknown", "unknown", &response).expect("usage");

        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(2));
    }

    #[test]
    fn root_usage_dominates_copied_stream_frames() {
        let response = json!({
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 2
            },
            "frames": [
                {
                    "usage": {
                        "prompt_tokens": 10,
                        "completion_tokens": 2
                    }
                }
            ]
        });

        let usage = extract_probe_usage("unknown", "unknown", &response).expect("usage");

        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(2));
    }
}
