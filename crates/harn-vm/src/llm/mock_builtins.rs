use crate::stdlib::json_to_vm_value;
use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::VmDictExt;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::{helpers, mock};

const LLM_MOCK_BUILTINS: &[&VmBuiltinDef] = &[
    &LLM_MOCK_BUILTIN_DEF,
    &LLM_MOCK_LOAD_JSONL_BUILTIN_DEF,
    &LLM_MOCK_CALLS_BUILTIN_DEF,
    &LLM_MOCK_RECEIPTS_BUILTIN_DEF,
    &LLM_MOCK_CLEAR_BUILTIN_DEF,
    &LLM_MOCK_PUSH_SCOPE_BUILTIN_DEF,
    &LLM_MOCK_POP_SCOPE_BUILTIN_DEF,
];

/// Register deterministic LLM mock builtins.
pub(super) fn register_llm_mock_builtins(vm: &mut Vm) {
    register_builtin_defs(vm, LLM_MOCK_BUILTINS);
}

/// Register a legacy v0 inline LLM mock response for tests.
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
    let value = helpers::vm_value_dict_to_json(config);
    let mock = crate::llm::parse_llm_mock_value(&value)
        .map_err(|error| VmError::Runtime(format!("llm_mock: {error}")))?;
    mock::push_inline_llm_mock(mock).map_err(VmError::Runtime)?;
    Ok(VmValue::Nil)
}

/// Atomically load a complete JSONL fixture document into the builtin mock
/// store. The caller owns filesystem access; this capability accepts text so
/// all hosts share the same parser without granting ambient reads to scripts.
#[harn_builtin(
    sig = "llm_mock_load_jsonl(text: string) -> dict",
    category = "llm.mock"
)]
fn llm_mock_load_jsonl_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let text = match args.first() {
        Some(VmValue::String(text)) => text,
        _ => {
            return Err(VmError::Runtime(
                "llm_mock_load_jsonl: expected fixture text".to_string(),
            ));
        }
    };
    let fixture = crate::llm::parse_llm_mocks_jsonl(text)
        .map_err(|error| VmError::Runtime(format!("llm_mock_load_jsonl: {error}")))?;
    let receipt = mock::install_builtin_llm_mock_fixture(fixture);
    let mut result = std::collections::BTreeMap::new();
    result.insert(
        "schema_version".to_string(),
        VmValue::Int(i64::from(receipt.schema_version)),
    );
    result.insert(
        "strict_scopes".to_string(),
        VmValue::Bool(receipt.strict_scopes),
    );
    result.insert("count".to_string(), VmValue::Int(receipt.count as i64));
    result.insert(
        "scopes".to_string(),
        VmValue::List(std::sync::Arc::new(
            receipt
                .scopes
                .into_iter()
                .map(|scope| VmValue::String(arcstr::ArcStr::from(scope)))
                .collect(),
        )),
    );
    Ok(VmValue::dict(result))
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
            dict.insert(
                "prefill".to_string(),
                c.prefill
                    .as_deref()
                    .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
                    .unwrap_or(VmValue::Nil),
            );
            VmValue::dict(dict)
        })
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(result)))
}

/// Return the scope-consumption receipts emitted since the last clear.
/// Each receipt is `{ scope, matched, entry_id, consume }`, letting a test
/// assert which scope bucket served each call without reading engine state.
#[harn_builtin(sig = "llm_mock_receipts() -> list", category = "llm.mock")]
fn llm_mock_receipts_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let receipts = mock::get_llm_mock_receipts();
    let result: Vec<VmValue> = receipts
        .iter()
        .map(|receipt| {
            let mut dict = std::collections::BTreeMap::new();
            dict.put_str("scope", receipt.scope.as_str());
            dict.insert("matched".to_string(), VmValue::Bool(receipt.matched));
            dict.put_str("entry_id", receipt.entry_id.as_str());
            dict.put_str("consume", receipt.consume.as_str());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn request(scope: Option<&str>) -> crate::llm::api::LlmRequestPayload {
        let mut options = crate::llm::api::options::base_opts("mock");
        options.messages = vec![serde_json::json!({"role": "user", "content": "prompt"})];
        options.mock_scope = scope.map(str::to_string);
        crate::llm::api::LlmRequestPayload::from(&options)
    }

    #[test]
    fn whole_document_load_replaces_atomically_and_scopes_restore() {
        mock::reset_llm_mock_state();
        let old = crate::llm::parse_llm_mock_value(&serde_json::json!({
            "match": "*",
            "text": "OLD"
        }))
        .expect("parse old fixture");
        mock::push_llm_mock(old);

        let mut out = String::new();
        let err = llm_mock_load_jsonl_builtin(
            &[VmValue::String(arcstr::ArcStr::from(
                "{\"schemaVersion\":1,\"strictScopes\":false}\n{not json}\n",
            ))],
            &mut out,
        )
        .expect_err("malformed document must fail before mutation");
        assert!(err.to_string().contains("invalid JSON"));
        assert_eq!(
            mock::mock_llm_response(&request(None))
                .expect("old fixture remains")
                .text,
            "OLD"
        );

        let receipt = llm_mock_load_jsonl_builtin(
            &[VmValue::String(arcstr::ArcStr::from(
                "{\"schemaVersion\":1,\"strictScopes\":true}\n\
                 {\"id\":\"judge-1\",\"scope\":\"judge\",\"consume\":\"sticky\",\"match\":\"*\",\"text\":\"JUDGE\"}\n",
            ))],
            &mut out,
        )
        .expect("valid document installs");
        let VmValue::Dict(receipt) = receipt else {
            panic!("load receipt must be a dict");
        };
        assert!(matches!(
            receipt.get("schema_version"),
            Some(VmValue::Int(1))
        ));
        assert!(matches!(
            receipt.get("strict_scopes"),
            Some(VmValue::Bool(true))
        ));
        assert!(matches!(receipt.get("count"), Some(VmValue::Int(1))));

        assert_eq!(
            mock::mock_llm_response(&request(Some("judge")))
                .expect("scoped fixture")
                .text,
            "JUDGE"
        );
        mock::push_llm_mock_scope();
        assert!(mock::pop_llm_mock_scope(), "fixture scope restores");
        assert_eq!(
            mock::mock_llm_response(&request(Some("judge")))
                .expect("restored scoped fixture")
                .text,
            "JUDGE"
        );

        let inline = VmValue::dict(std::collections::BTreeMap::from([(
            "text".to_string(),
            VmValue::String(arcstr::ArcStr::from("must not mix")),
        )]));
        let error = llm_mock_builtin(&[inline], &mut out)
            .expect_err("inline v0 entry must not mutate a v1 fixture");
        assert!(error.to_string().contains("active versioned fixture"));
        mock::reset_llm_mock_state();
    }

    #[test]
    fn inline_mock_uses_the_canonical_v0_decoder() {
        mock::reset_llm_mock_state();
        let mut out = String::new();
        let inline = VmValue::dict(std::collections::BTreeMap::from([
            (
                "match".to_string(),
                VmValue::String(arcstr::ArcStr::from("*")),
            ),
            (
                "scope".to_string(),
                VmValue::String(arcstr::ArcStr::from("judge")),
            ),
            (
                "text".to_string(),
                VmValue::String(arcstr::ArcStr::from("V0")),
            ),
        ]));
        llm_mock_builtin(&[inline], &mut out).expect("inline v0 mock");

        // v0 ignores the newer scope annotation and retains its historical
        // default-queue behavior.
        assert_eq!(
            mock::mock_llm_response(&request(Some("judge")))
                .expect("v0 fallback")
                .text,
            "V0"
        );
        mock::reset_llm_mock_state();
    }
}
