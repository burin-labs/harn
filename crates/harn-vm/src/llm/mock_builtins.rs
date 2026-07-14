use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::VmDictExt;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::{helpers, mock};

const LLM_MOCK_BUILTINS: &[&VmBuiltinDef] = &[
    &LLM_MOCK_BUILTIN_DEF,
    &LLM_MOCK_CALLS_BUILTIN_DEF,
    &LLM_MOCK_CLEAR_BUILTIN_DEF,
    &LLM_MOCK_PUSH_SCOPE_BUILTIN_DEF,
    &LLM_MOCK_POP_SCOPE_BUILTIN_DEF,
];

/// Register llm_mock / llm_mock_calls / llm_mock_clear builtins.
pub(super) fn register_llm_mock_builtins(vm: &mut Vm) {
    register_builtin_defs(vm, LLM_MOCK_BUILTINS);
}

/// Register a deterministic LLM mock response for tests.
#[harn_builtin(sig = "llm_mock(config: dict) -> nil", category = "llm.mock")]
fn llm_mock_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let config = match args.first() {
        Some(VmValue::Dict(d)) => d,
        _ => {
            return Err(VmError::Runtime(
                "llm_mock: expected a dict argument".to_string(),
            ))
        }
    };

    let text = config.get("text").map(|v| v.display()).unwrap_or_default();

    let tool_calls = match config.get("tool_calls") {
        Some(VmValue::List(list)) => list
            .iter()
            .map(helpers::vm_value_to_json)
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let logprobs = match config.get("logprobs") {
        Some(VmValue::List(list)) => list
            .iter()
            .map(helpers::vm_value_to_json)
            .collect::<Vec<_>>(),
        Some(VmValue::Nil) | None => Vec::new(),
        _ => {
            return Err(VmError::Runtime(
                "llm_mock: logprobs must be a list of token logprob dicts".to_string(),
            ))
        }
    };
    let blocks = match config.get("blocks") {
        Some(VmValue::List(list)) => Some(
            list.iter()
                .map(helpers::vm_value_to_json)
                .collect::<Vec<_>>(),
        ),
        Some(VmValue::Nil) | None => None,
        _ => {
            return Err(VmError::Runtime(
                "llm_mock: blocks must be a list of response blocks".to_string(),
            ))
        }
    };

    // Optional ordered visible-text chunks for streaming callers. Each entry
    // is emitted as a separate delta by the streaming delta pump; non-streaming
    // callers see only the flat `text` (derived from the concatenation when
    // `text` is omitted).
    let stream_chunks = match config.get("stream_chunks") {
        Some(VmValue::List(list)) => {
            let mut chunks = Vec::with_capacity(list.len());
            for item in list.iter() {
                match item {
                    VmValue::String(s) => chunks.push(s.to_string()),
                    other => {
                        return Err(VmError::Runtime(format!(
                            "llm_mock: stream_chunks entries must be strings; got {}",
                            other.type_name()
                        )))
                    }
                }
            }
            chunks
        }
        Some(VmValue::Nil) | None => Vec::new(),
        _ => {
            return Err(VmError::Runtime(
                "llm_mock: stream_chunks must be a list of strings".to_string(),
            ))
        }
    };

    let match_pattern = config.get("match").and_then(|v| {
        if matches!(v, VmValue::Nil) {
            None
        } else {
            Some(v.display())
        }
    });
    let consume_on_match = matches!(config.get("consume_match"), Some(VmValue::Bool(true)));

    let input_tokens = config.get("input_tokens").and_then(|v| v.as_int());
    let output_tokens = config.get("output_tokens").and_then(|v| v.as_int());
    let cache_read_tokens = config.get("cache_read_tokens").and_then(|v| v.as_int());
    let cache_write_tokens = config
        .get("cache_write_tokens")
        .and_then(|v| v.as_int())
        .or_else(|| {
            config
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_int())
        });
    let thinking = config.get("thinking").and_then(|v| {
        if matches!(v, VmValue::Nil) {
            None
        } else {
            Some(v.display())
        }
    });
    let thinking_summary = config.get("thinking_summary").and_then(|v| {
        if matches!(v, VmValue::Nil) {
            None
        } else {
            Some(v.display())
        }
    });
    let stop_reason = config.get("stop_reason").and_then(|v| {
        if matches!(v, VmValue::Nil) {
            None
        } else {
            Some(v.display())
        }
    });
    let model = config
        .get("model")
        .map(|v| v.display())
        .unwrap_or_else(|| "mock".to_string());
    let provider = config.get("provider").and_then(|v| {
        if matches!(v, VmValue::Nil) {
            None
        } else {
            Some(v.display())
        }
    });

    // Optional error injection. Category-only mocks surface as
    // categorized provider failures; provider-envelope mocks also keep
    // status/kind/reason on the final thrown dict.
    let error = match config.get("error") {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::Dict(err_dict)) => {
            let category = optional_display_field(err_dict, "category");
            let message = optional_display_field(err_dict, "message");
            let status = match err_dict.get("status") {
                None | Some(VmValue::Nil) => None,
                Some(value) => match value.as_int() {
                    Some(n) => Some(
                        mock::validate_mock_error_status(n)
                            .map_err(|error| VmError::Runtime(format!("llm_mock: {error}")))?,
                    ),
                    None => {
                        return Err(VmError::Runtime(
                            "llm_mock: error.status must be an HTTP status code".to_string(),
                        ));
                    }
                },
            };
            let kind = optional_display_field(err_dict, "kind");
            let reason = optional_display_field(err_dict, "reason");
            let retry_after_ms = match err_dict.get("retry_after_ms") {
                None | Some(VmValue::Nil) => None,
                Some(v) => match v.as_int() {
                    Some(n) if n >= 0 => Some(n as u64),
                    _ => {
                        return Err(VmError::Runtime(
                            "llm_mock: error.retry_after_ms must be a non-negative int".to_string(),
                        ));
                    }
                },
            };
            Some(
                mock::build_mock_error(category, message, status, kind, reason, retry_after_ms)
                    .map_err(|error| VmError::Runtime(format!("llm_mock: {error}")))?,
            )
        }
        _ => {
            return Err(VmError::Runtime(
                "llm_mock: error must be a dict {category?, message?, status?, kind?, reason?, retry_after_ms?}".to_string(),
            ));
        }
    };

    mock::push_llm_mock(mock::LlmMock {
        text,
        tool_calls,
        raw_tool_calls: Vec::new(),
        match_pattern,
        consume_on_match,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        thinking,
        thinking_summary,
        stop_reason,
        model,
        provider,
        blocks,
        logprobs,
        error,
        stream_chunks,
    });
    Ok(VmValue::Nil)
}

fn optional_display_field(dict: &crate::value::DictMap, key: &str) -> Option<String> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => None,
        Some(value) => Some(value.display()),
    }
}

/// Return recorded LLM mock calls.
#[harn_builtin(sig = "llm_mock_calls() -> list", category = "llm.mock")]
fn llm_mock_calls_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let calls = mock::get_llm_mock_calls();
    let result: Vec<VmValue> = calls
        .iter()
        .map(|c| {
            let mut dict = std::collections::BTreeMap::new();
            dict.put_str("api_mode", c.api_mode.as_str());
            let messages: Vec<VmValue> = c.messages.iter().map(json_to_vm_value).collect();
            dict.insert(
                "messages".to_string(),
                VmValue::List(std::sync::Arc::new(messages)),
            );
            dict.insert(
                "system".to_string(),
                match &c.system {
                    Some(s) => VmValue::String(arcstr::ArcStr::from(s.as_str())),
                    None => VmValue::Nil,
                },
            );
            dict.insert(
                "tools".to_string(),
                match &c.tools {
                    Some(t) => {
                        let tools: Vec<VmValue> = t.iter().map(json_to_vm_value).collect();
                        VmValue::List(std::sync::Arc::new(tools))
                    }
                    None => VmValue::Nil,
                },
            );
            dict.insert(
                "provider_tools".to_string(),
                match &c.provider_tools {
                    Some(t) => {
                        let tools: Vec<VmValue> = t.iter().map(json_to_vm_value).collect();
                        VmValue::List(std::sync::Arc::new(tools))
                    }
                    None => VmValue::Nil,
                },
            );
            dict.insert(
                "tool_choice".to_string(),
                match &c.tool_choice {
                    Some(choice) => json_to_vm_value(choice),
                    None => VmValue::Nil,
                },
            );
            dict.insert(
                "output_format".to_string(),
                json_to_vm_value(&c.output_format),
            );
            dict.insert("thinking".to_string(), json_to_vm_value(&c.thinking));
            dict.insert(
                "previous_response_id".to_string(),
                c.previous_response_id
                    .as_deref()
                    .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
                    .unwrap_or(VmValue::Nil),
            );
            dict.insert(
                "store".to_string(),
                c.store.map(VmValue::Bool).unwrap_or(VmValue::Nil),
            );
            dict.insert(
                "background".to_string(),
                c.background.map(VmValue::Bool).unwrap_or(VmValue::Nil),
            );
            dict.insert(
                "truncation".to_string(),
                c.truncation
                    .as_deref()
                    .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
                    .unwrap_or(VmValue::Nil),
            );
            dict.insert(
                "compact".to_string(),
                c.compact.map(VmValue::Bool).unwrap_or(VmValue::Nil),
            );
            dict.insert(
                "include".to_string(),
                c.include
                    .as_ref()
                    .map(|items| {
                        VmValue::List(std::sync::Arc::new(
                            items
                                .iter()
                                .map(|item| VmValue::String(arcstr::ArcStr::from(item.as_str())))
                                .collect(),
                        ))
                    })
                    .unwrap_or(VmValue::Nil),
            );
            dict.insert(
                "max_tool_calls".to_string(),
                c.max_tool_calls.map(VmValue::Int).unwrap_or(VmValue::Nil),
            );
            VmValue::dict(dict)
        })
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(result)))
}

/// Clear deterministic LLM mocks and recorded calls.
#[harn_builtin(sig = "llm_mock_clear() -> nil", category = "llm.mock")]
fn llm_mock_clear_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    mock::reset_llm_mock_state();
    Ok(VmValue::Nil)
}

/// Push an isolated LLM mock scope.
#[harn_builtin(sig = "llm_mock_push_scope() -> nil", category = "llm.mock")]
fn llm_mock_push_scope_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    mock::push_llm_mock_scope();
    Ok(VmValue::Nil)
}

/// Pop the current isolated LLM mock scope.
#[harn_builtin(sig = "llm_mock_pop_scope() -> nil", category = "llm.mock")]
fn llm_mock_pop_scope_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if !mock::pop_llm_mock_scope() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "llm_mock_pop_scope: no scope to pop",
        ))));
    }
    Ok(VmValue::Nil)
}
