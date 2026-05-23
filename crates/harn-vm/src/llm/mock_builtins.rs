use std::rc::Rc;

use crate::stdlib::json_to_vm_value;
use crate::stdlib::registration::{
    register_builtin_group, register_deferred_builtin_group, BuiltinGroup, SyncBuiltin,
};
use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity};

use super::{helpers, mock};

const LLM_MOCK_SYNC_PRIMITIVES: &[SyncBuiltin] = &[
    SyncBuiltin::new("llm_mock", llm_mock_builtin)
        .signature("llm_mock(config)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Register a deterministic LLM mock response for tests."),
    SyncBuiltin::new("llm_mock_calls", llm_mock_calls_builtin)
        .signature("llm_mock_calls()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Return recorded LLM mock calls."),
    SyncBuiltin::new("llm_mock_clear", llm_mock_clear_builtin)
        .signature("llm_mock_clear()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Clear deterministic LLM mocks and recorded calls."),
    SyncBuiltin::new("llm_mock_push_scope", llm_mock_push_scope_builtin)
        .signature("llm_mock_push_scope()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Push an isolated LLM mock scope."),
    SyncBuiltin::new("llm_mock_pop_scope", llm_mock_pop_scope_builtin)
        .signature("llm_mock_pop_scope()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Pop the current isolated LLM mock scope."),
];

const LLM_MOCK_PRIMITIVES: BuiltinGroup<'static> = BuiltinGroup::new()
    .category("llm.mock")
    .sync(LLM_MOCK_SYNC_PRIMITIVES);

/// Register llm_mock / llm_mock_calls / llm_mock_clear builtins.
pub(super) fn register_llm_mock_builtins(vm: &mut Vm) {
    register_builtin_group(vm, LLM_MOCK_PRIMITIVES);
}

pub(super) fn register_deferred_llm_mock_builtins(vm: &mut Vm, registrar: fn(&mut Vm)) {
    register_deferred_builtin_group(vm, LLM_MOCK_PRIMITIVES, registrar);
}

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

    // Optional error injection: {error: {category, message,
    // retry_after_ms?}}. When present the mock short-circuits the
    // provider call and surfaces as `VmError::CategorizedError`,
    // making it observable via `error_category`, the `llm_call`
    // thrown dict, and the `llm_call_safe` envelope.
    let error = match config.get("error") {
        None | Some(VmValue::Nil) => None,
        Some(VmValue::Dict(err_dict)) => {
            let category_str = err_dict
                .get("category")
                .map(|v| v.display())
                .unwrap_or_default();
            if category_str.is_empty() {
                return Err(VmError::Runtime(
                    "llm_mock: error.category is required".to_string(),
                ));
            }
            let category = crate::value::ErrorCategory::parse(&category_str);
            // Reject typos loudly: `parse` falls back to Generic on
            // unknown input. Let `"generic"` through; anything else
            // that fell back is a typo.
            if category.as_str() != category_str {
                return Err(VmError::Runtime(format!(
                    "llm_mock: unknown error category `{category_str}`",
                )));
            }
            let message = err_dict
                .get("message")
                .map(|v| v.display())
                .unwrap_or_default();
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
            Some(mock::MockError {
                category,
                message,
                retry_after_ms,
            })
        }
        _ => {
            return Err(VmError::Runtime(
                "llm_mock: error must be a dict {category, message, retry_after_ms?}".to_string(),
            ));
        }
    };

    mock::push_llm_mock(mock::LlmMock {
        text,
        tool_calls,
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
    });
    Ok(VmValue::Nil)
}

fn llm_mock_calls_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let calls = mock::get_llm_mock_calls();
    let result: Vec<VmValue> = calls
        .iter()
        .map(|c| {
            let mut dict = std::collections::BTreeMap::new();
            let messages: Vec<VmValue> = c.messages.iter().map(json_to_vm_value).collect();
            dict.insert("messages".to_string(), VmValue::List(Rc::new(messages)));
            dict.insert(
                "system".to_string(),
                match &c.system {
                    Some(s) => VmValue::String(Rc::from(s.as_str())),
                    None => VmValue::Nil,
                },
            );
            dict.insert(
                "tools".to_string(),
                match &c.tools {
                    Some(t) => {
                        let tools: Vec<VmValue> = t.iter().map(json_to_vm_value).collect();
                        VmValue::List(Rc::new(tools))
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
            dict.insert("thinking".to_string(), json_to_vm_value(&c.thinking));
            VmValue::Dict(Rc::new(dict))
        })
        .collect();
    Ok(VmValue::List(Rc::new(result)))
}

fn llm_mock_clear_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    mock::reset_llm_mock_state();
    Ok(VmValue::Nil)
}

fn llm_mock_push_scope_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    mock::push_llm_mock_scope();
    Ok(VmValue::Nil)
}

fn llm_mock_pop_scope_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if !mock::pop_llm_mock_scope() {
        return Err(VmError::Thrown(VmValue::String(Rc::from(
            "llm_mock_pop_scope: no scope to pop",
        ))));
    }
    Ok(VmValue::Nil)
}
