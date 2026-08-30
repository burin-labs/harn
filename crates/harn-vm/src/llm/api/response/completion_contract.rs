//! One typed boundary for provider completions that consumed tokens without a
//! dispatchable result. Complete and streaming parsers share this contract.

use crate::llm::usage::ProviderUsageReceipt;
use crate::value::{VmError, VmValue};

/// Provider facts retained when parsing proves that a response completed but
/// produced no usable generation. The parser owns wire-shape extraction; the
/// observed-call boundary owns call correlation and transcript ordering.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProviderResponseEnvelope {
    response_id: Option<String>,
    stop_reason: Option<String>,
    content_block_types: Vec<String>,
    usage: ProviderUsageReceipt,
}

impl ProviderResponseEnvelope {
    pub(crate) fn new(
        response_id: Option<&str>,
        stop_reason: Option<&str>,
        content_block_types: Vec<String>,
        usage: ProviderUsageReceipt,
    ) -> Self {
        Self {
            response_id: response_id
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            stop_reason: stop_reason.map(str::to_string),
            content_block_types,
            usage,
        }
    }

    pub(crate) fn response_id(&self) -> Option<&str> {
        self.response_id.as_deref()
    }

    pub(crate) fn stop_reason(&self) -> Option<&str> {
        self.stop_reason.as_deref()
    }

    pub(crate) fn content_block_count(&self) -> usize {
        self.content_block_types.len()
    }

    pub(crate) fn content_block_types(&self) -> &[String] {
        &self.content_block_types
    }

    pub(crate) fn usage(&self) -> &ProviderUsageReceipt {
        &self.usage
    }

    fn to_vm_value(&self) -> VmValue {
        VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("response_id"),
                self.response_id.as_deref().map_or(VmValue::Nil, |value| {
                    VmValue::String(arcstr::ArcStr::from(value))
                }),
            ),
            (
                crate::value::intern_key("stop_reason"),
                self.stop_reason.as_deref().map_or(VmValue::Nil, |value| {
                    VmValue::String(arcstr::ArcStr::from(value))
                }),
            ),
            (
                crate::value::intern_key("content_blocks"),
                VmValue::dict(crate::value::DictMap::from_iter([
                    (
                        crate::value::intern_key("count"),
                        VmValue::Int(i64::try_from(self.content_block_count()).unwrap_or(i64::MAX)),
                    ),
                    (
                        crate::value::intern_key("types"),
                        VmValue::List(std::sync::Arc::new(
                            self.content_block_types
                                .iter()
                                .map(|value| VmValue::String(arcstr::ArcStr::from(value.as_str())))
                                .collect(),
                        )),
                    ),
                ])),
            ),
            (crate::value::intern_key("usage"), self.usage.to_vm_value()),
        ]))
    }

    pub(crate) fn from_error(error: &VmError) -> Option<Self> {
        let VmError::Thrown(VmValue::Dict(error_fields)) = error else {
            return None;
        };
        let VmValue::Dict(fields) = error_fields.get("provider_response")? else {
            return None;
        };
        let response_id = match fields.get("response_id")? {
            VmValue::Nil => None,
            VmValue::String(value) => Some(value.to_string()),
            _ => return None,
        };
        let stop_reason = match fields.get("stop_reason")? {
            VmValue::Nil => None,
            VmValue::String(value) => Some(value.to_string()),
            _ => return None,
        };
        let VmValue::Dict(content_blocks) = fields.get("content_blocks")? else {
            return None;
        };
        let count = content_blocks
            .get("count")
            .and_then(VmValue::as_int)
            .filter(|count| *count >= 0)
            .and_then(|count| usize::try_from(count).ok())?;
        let VmValue::List(types) = content_blocks.get("types")? else {
            return None;
        };
        let content_block_types = types
            .iter()
            .map(|value| match value {
                VmValue::String(value) => Some(value.to_string()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()?;
        if content_block_types.len() != count {
            return None;
        }
        let usage = ProviderUsageReceipt::from_vm_value(fields.get("usage")?)?;
        Some(Self {
            response_id,
            stop_reason,
            content_block_types,
            usage,
        })
    }
}

/// Preserve the raw block discriminator at the parser seam. Missing or
/// malformed discriminators remain visible as their JSON shape rather than
/// disappearing into an empty list that would look like no blocks arrived.
pub(crate) fn provider_content_block_types(blocks: Option<&[serde_json::Value]>) -> Vec<String> {
    blocks
        .unwrap_or_default()
        .iter()
        .map(|block| {
            block
                .get("type")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| json_shape(block).to_string())
        })
        .collect()
}

pub(crate) fn openai_responses_content_block_types(output: &[serde_json::Value]) -> Vec<String> {
    output
        .iter()
        .flat_map(|item| {
            if item.get("type").and_then(serde_json::Value::as_str) == Some("message") {
                return provider_content_block_types(
                    item.get("content")
                        .and_then(serde_json::Value::as_array)
                        .map(Vec::as_slice),
                );
            }
            provider_content_block_types(Some(std::slice::from_ref(item)))
        })
        .collect()
}

pub(crate) fn openai_message_content_block_types(message: &serde_json::Value) -> Vec<String> {
    let mut types = match message.get("content") {
        Some(serde_json::Value::Array(blocks)) => {
            provider_content_block_types(Some(blocks.as_slice()))
        }
        Some(serde_json::Value::String(_)) => vec!["text".to_string()],
        Some(serde_json::Value::Object(block)) => provider_content_block_types(Some(
            std::slice::from_ref(&serde_json::Value::Object(block.clone())),
        )),
        Some(serde_json::Value::Null) | None => Vec::new(),
        Some(other) => provider_content_block_types(Some(std::slice::from_ref(other))),
    };
    if message.get("refusal").is_some_and(|value| !value.is_null()) {
        types.push("refusal".to_string());
    }
    types
}

fn json_shape(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Structural signals for a finished LLM turn. They are derived from the
/// parsed response and outbound request, never model-name branching.
pub(crate) struct CompletionContractSignals<'a> {
    pub stop_reason: Option<&'a str>,
    pub output_tokens: i64,
    pub tools_offered: bool,
    pub tool_call_count: usize,
    pub has_tool_search_block: bool,
    pub text: &'a str,
}

/// Detect a clean, billed tool turn with neither a tool call nor committed
/// text. A length stop is always recoverable truncation, not this violation.
pub(crate) fn is_billed_noncommittal_completion(signals: &CompletionContractSignals) -> bool {
    !is_length_stop_reason(signals.stop_reason)
        && signals.output_tokens > 0
        && signals.tools_offered
        && signals.tool_call_count == 0
        && !signals.has_tool_search_block
        && signals.text.trim().is_empty()
}

pub(crate) fn is_length_stop_reason(stop_reason: Option<&str>) -> bool {
    stop_reason.is_some_and(super::super::result::stop_reason_is_length)
}

/// Builds the actionable error for a billed completion that committed neither
/// an answer nor a dispatchable tool call.
pub(crate) fn billed_noncommittal_completion_error(
    provider: &str,
    model: &str,
    usage: ProviderUsageReceipt,
) -> VmError {
    let completion_tokens = usage
        .output_tokens()
        .map_or_else(|| "unknown".to_string(), |tokens| tokens.to_string());
    provider_completion_error(
        provider,
        model,
        "unproductive_completion",
        "unproductive_completion",
        "billed_noncommittal",
        usage,
        None,
        format!(
            "provider {provider} model {model} returned billed output \
             (completion_tokens={completion_tokens}) with no dispatchable tool call or answer \
             (upstream contract violation): the model finished cleanly but committed neither a \
             tool call nor visible text. This usually means the route serialized \
             the action only in a private reasoning channel or returned an empty committed \
             message. For OpenRouter aggregate routes, consider provider_route_denylist or \
             provider_order; for first-party routes, prefer a Harn text/json tool format or \
             disable auto reasoning when the capability row documents it.",
        ),
    )
}

/// Builds the typed empty-generation error shared by complete and stream
/// parsers. Dispatch branches on `code`; the diagnostic message may evolve.
pub(crate) fn empty_generation_error(
    provider: &str,
    model: &str,
    response: ProviderResponseEnvelope,
    message: String,
) -> VmError {
    let usage = response.usage.clone();
    provider_completion_error(
        provider,
        model,
        "empty_generation",
        "empty_generation",
        "empty_generation",
        usage,
        Some(response),
        message,
    )
}

fn provider_completion_error(
    provider: &str,
    model: &str,
    code: &str,
    reason: &str,
    completion_kind: &str,
    usage: ProviderUsageReceipt,
    response: Option<ProviderResponseEnvelope>,
    message: String,
) -> VmError {
    let mut fields = crate::value::DictMap::from_iter([
        (
            "category".to_string(),
            VmValue::String(arcstr::ArcStr::from("server_error")),
        ),
        (
            "code".to_string(),
            VmValue::String(arcstr::ArcStr::from(code)),
        ),
        (
            "reason".to_string(),
            VmValue::String(arcstr::ArcStr::from(reason)),
        ),
        (
            "completion_kind".to_string(),
            VmValue::String(arcstr::ArcStr::from(completion_kind)),
        ),
        (
            "provider".to_string(),
            VmValue::String(arcstr::ArcStr::from(provider)),
        ),
        (
            "model".to_string(),
            VmValue::String(arcstr::ArcStr::from(model)),
        ),
        (
            "message".to_string(),
            VmValue::String(arcstr::ArcStr::from(message)),
        ),
        ("provider_usage".to_string(), usage.to_vm_value()),
    ]);
    if let Some(output_tokens) = usage.output_tokens() {
        fields.insert(
            crate::value::intern_key("output_tokens"),
            VmValue::Int(output_tokens),
        );
    }
    if let Some(response) = response {
        fields.insert(
            crate::value::intern_key("provider_response"),
            response.to_vm_value(),
        );
    }
    VmError::Thrown(VmValue::dict(fields))
}
