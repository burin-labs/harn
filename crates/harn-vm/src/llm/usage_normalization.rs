use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolProbeUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
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
    let cost_usd = totals
        .input_tokens
        .zip(totals.output_tokens)
        .and_then(|(input, output)| {
            crate::llm::cost::pricing_aware_call_cost(provider, model, input, output)
        });
    Some(ToolProbeUsage {
        input_tokens: totals.input_tokens,
        output_tokens: totals.output_tokens,
        cost_usd,
    })
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
