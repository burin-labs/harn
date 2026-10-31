//! Inspectable branch results with live-only accounting.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{EventTape, LlmResult, TapeRecordKind};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordedModelCall {
    pub index: u64,
    pub replayed: bool,
    pub request_digest: String,
    pub text: String,
    pub tool_calls: Vec<Value>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Replayed calls cost zero. Unpriced live calls remain unknown.
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordedToolCall {
    pub after_call: u64,
    pub name: String,
    pub args_digest: String,
    pub outcome: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunTapeSummary {
    pub calls: Vec<RecordedModelCall>,
    pub tools: Vec<RecordedToolCall>,
    /// Last recorded effect for each path. None means deletion.
    pub files: BTreeMap<String, Option<String>>,
    pub live_calls: u64,
    pub live_input_tokens: i64,
    pub live_output_tokens: i64,
    pub live_cost_usd: Option<f64>,
}

/// Count only fresh inference as spend. Missing model calls and malformed
/// payloads are errors, not empty successful comparisons.
pub fn summarize_run_tape(tape: &EventTape, served_calls: u64) -> Result<RunTapeSummary, String> {
    let mut summary = RunTapeSummary {
        calls: Vec::new(),
        tools: Vec::new(),
        files: BTreeMap::new(),
        live_calls: 0,
        live_input_tokens: 0,
        live_output_tokens: 0,
        live_cost_usd: Some(0.0),
    };
    for record in &tape.records {
        match &record.kind {
            TapeRecordKind::LlmCall {
                call_index,
                request_digest,
                response,
                ..
            } => {
                let index = call_index.ok_or("run summary needs numbered model calls")?;
                if index != summary.calls.len() as u64 + 1 {
                    return Err("run summary needs a complete ordered model-call history".into());
                }
                let result: LlmResult = serde_json::from_slice(&tape.resolve_payload(response)?)
                    .map_err(|error| format!("recorded model response: {error}"))?;
                let replayed = index <= served_calls;
                let cost_usd = if replayed {
                    Some(0.0)
                } else {
                    result.usage().cost_usd
                };
                if !replayed {
                    summary.live_calls += 1;
                    summary.live_input_tokens = summary
                        .live_input_tokens
                        .checked_add(result.input_tokens)
                        .ok_or("input token total overflow")?;
                    summary.live_output_tokens = summary
                        .live_output_tokens
                        .checked_add(result.output_tokens)
                        .ok_or("output token total overflow")?;
                    summary.live_cost_usd = summary
                        .live_cost_usd
                        .zip(cost_usd)
                        .map(|(total, cost)| total + cost);
                }
                summary.calls.push(RecordedModelCall {
                    index,
                    replayed,
                    request_digest: request_digest.clone(),
                    text: result.text,
                    tool_calls: result.tool_calls,
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    cost_usd,
                });
            }
            TapeRecordKind::ToolCall {
                after_calls,
                tool,
                args_digest,
                outcome,
                effects,
                ..
            } => {
                let outcome = serde_json::from_slice(&tape.resolve_payload(outcome)?)
                    .map_err(|error| format!("recorded tool outcome: {error}"))?;
                summary.tools.push(RecordedToolCall {
                    after_call: *after_calls,
                    name: tool.clone(),
                    args_digest: args_digest.clone(),
                    outcome,
                });
                for effect in effects {
                    summary.files.insert(
                        effect.path.clone(),
                        effect
                            .content
                            .as_ref()
                            .map(|content| content.content_hash().to_owned()),
                    );
                }
            }
            _ => {}
        }
    }
    if summary.calls.is_empty() || served_calls > summary.calls.len() as u64 {
        return Err("run has no complete model-call evidence for this replay boundary".into());
    }
    Ok(summary)
}
