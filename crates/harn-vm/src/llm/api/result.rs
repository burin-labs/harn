//! `LlmResult` and the Harn-facing dict builder for `llm_call` return
//! values, plus the mock-provider completion response.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::VmValue;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub(crate) struct LlmResult {
    pub text: String,
    pub tool_calls: Vec<serde_json::Value>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Prompt tokens served from the provider's cache (when supported).
    /// Anthropic: `usage.cache_read_input_tokens`.
    /// OpenAI: `usage.prompt_tokens_details.cached_tokens`.
    /// OpenRouter passthrough for Anthropic: `usage.cache_read_input_tokens`.
    /// Defaults to 0 when the provider doesn't report it.
    pub cache_read_tokens: i64,
    /// Prompt tokens written to the provider's cache on this request
    /// (Anthropic `usage.cache_creation_input_tokens`). Helps distinguish
    /// "warm-up" calls from cache hits.
    pub cache_write_tokens: i64,
    pub model: String,
    pub provider: String,
    pub thinking: Option<String>,
    pub stop_reason: Option<String>,
    pub blocks: Vec<serde_json::Value>,
}

pub(crate) fn vm_build_llm_result(
    result: &LlmResult,
    parsed_json: Option<VmValue>,
    transcript: Option<VmValue>,
    tools_val: Option<&VmValue>,
) -> VmValue {
    use crate::stdlib::json_to_vm_value;

    let mut dict = BTreeMap::new();
    dict.insert(
        "text".to_string(),
        VmValue::String(Rc::from(result.text.as_str())),
    );
    dict.insert(
        "model".to_string(),
        VmValue::String(Rc::from(result.model.as_str())),
    );
    dict.insert(
        "provider".to_string(),
        VmValue::String(Rc::from(result.provider.as_str())),
    );
    dict.insert(
        "input_tokens".to_string(),
        VmValue::Int(result.input_tokens),
    );
    dict.insert(
        "output_tokens".to_string(),
        VmValue::Int(result.output_tokens),
    );
    // Cache accounting (0 when provider doesn't report cache info).
    dict.insert(
        "cache_read_tokens".to_string(),
        VmValue::Int(result.cache_read_tokens),
    );
    dict.insert(
        "cache_write_tokens".to_string(),
        VmValue::Int(result.cache_write_tokens),
    );

    if let Some(json_val) = parsed_json {
        dict.insert("data".to_string(), json_val);
    }

    // Always run the tagged-protocol parser so canonical_text / violations /
    // done_marker are available even with no tool registry. It's cheap and a
    // no-op when the text has no tags. Native provider tool calls are
    // authoritative and take precedence over text-parsed calls.
    let tagged = Some(crate::llm::tools::parse_text_tool_calls_with_tools(
        &result.text,
        tools_val,
    ));

    let merged_tool_calls: Vec<serde_json::Value> = if !result.tool_calls.is_empty() {
        result.tool_calls.clone()
    } else if let Some(parse) = tagged.as_ref() {
        parse.calls.clone()
    } else {
        Vec::new()
    };
    if !merged_tool_calls.is_empty() {
        let calls: Vec<VmValue> = merged_tool_calls.iter().map(json_to_vm_value).collect();
        dict.insert("tool_calls".to_string(), VmValue::List(Rc::new(calls)));
    }

    if let Some(parse) = tagged.as_ref() {
        if !parse.violations.is_empty() {
            let violations: Vec<VmValue> = parse
                .violations
                .iter()
                .map(|v| VmValue::String(Rc::from(v.as_str())))
                .collect();
            dict.insert(
                "protocol_violations".to_string(),
                VmValue::List(Rc::new(violations)),
            );
        }
        if !parse.errors.is_empty() {
            let errors: Vec<VmValue> = parse
                .errors
                .iter()
                .map(|e| VmValue::String(Rc::from(e.as_str())))
                .collect();
            dict.insert(
                "tool_parse_errors".to_string(),
                VmValue::List(Rc::new(errors)),
            );
        }
        if let Some(ref body) = parse.done_marker {
            dict.insert(
                "done_marker".to_string(),
                VmValue::String(Rc::from(body.as_str())),
            );
        }
        if !parse.canonical.is_empty() {
            dict.insert(
                "canonical_text".to_string(),
                VmValue::String(Rc::from(parse.canonical.as_str())),
            );
        }
        // Always emit `prose` (fall back to raw text) so callers have a
        // single reliable "the answer" key regardless of whether the model
        // used the tagged protocol.
        let prose = if parse.prose.is_empty() {
            result.text.clone()
        } else {
            parse.prose.clone()
        };
        dict.insert(
            "prose".to_string(),
            VmValue::String(Rc::from(prose.as_str())),
        );
    }

    if let Some(ref thinking) = result.thinking {
        dict.insert(
            "thinking".to_string(),
            VmValue::String(Rc::from(thinking.as_str())),
        );
        dict.insert(
            "private_reasoning".to_string(),
            VmValue::String(Rc::from(thinking.as_str())),
        );
    }

    if let Some(ref stop_reason) = result.stop_reason {
        dict.insert(
            "stop_reason".to_string(),
            VmValue::String(Rc::from(stop_reason.as_str())),
        );
    }

    if let Some(transcript) = transcript {
        dict.insert("transcript".to_string(), transcript);
    }

    // Prose with fenceless TS tool-call expressions stripped. Agent_loop
    // applies the same semantics on its final iteration.
    let visible_text = if tools_val.is_some() && result.tool_calls.is_empty() {
        let parse_result =
            crate::llm::tools::parse_text_tool_calls_with_tools(&result.text, tools_val);
        parse_result.prose
    } else {
        result.text.clone()
    };
    dict.insert(
        "visible_text".to_string(),
        VmValue::String(Rc::from(visible_text.as_str())),
    );
    dict.insert(
        "blocks".to_string(),
        VmValue::List(Rc::new(
            result
                .blocks
                .iter()
                .map(json_to_vm_value)
                .collect::<Vec<_>>(),
        )),
    );

    VmValue::Dict(Rc::new(dict))
}

pub(super) fn mock_completion_response(prefix: &str, suffix: Option<&str>) -> LlmResult {
    let suffix = suffix.unwrap_or_default();
    let text = format!(
        "Mock completion after {} chars{}",
        prefix.chars().count(),
        if suffix.is_empty() {
            String::new()
        } else {
            format!(" before {} chars", suffix.chars().count())
        }
    );
    LlmResult {
        text: text.clone(),
        tool_calls: Vec::new(),
        input_tokens: (prefix.len() + suffix.len()) as i64,
        output_tokens: 16,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: "mock".to_string(),
        provider: "mock".to_string(),
        thinking: None,
        stop_reason: Some("stop".to_string()),
        blocks: vec![serde_json::json!({
            "type": "output_text",
            "text": text,
            "visibility": "public",
        })],
    }
}
