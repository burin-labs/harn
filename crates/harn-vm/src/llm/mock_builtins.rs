use crate::schema::json_to_vm_value;
use crate::value::VmDictExt;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;
use harn_builtin_macros::harn_builtin;

use super::{helpers, mock};

/// Register deterministic LLM mock builtins.
pub(super) fn register_llm_mock_builtins(vm: &mut Vm) {
    harn_vm_core::builtin_support::register_module_builtins(vm, module_path!());
}

/// Whether installed replay policy prevents provider I/O even after exhaustion.
#[harn_builtin(
    exposure = "harness.llm.provider_io_disposition",
    effects = ["state.read@const=llm-fixture"],
    sig = "provider_io_disposition() -> {provider_io: bool, source: \"cli_replay\" | \"builtin_replay\" | \"possible\"}",
    category = "llm.mock"
)]
fn provider_io_disposition_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    if !args.is_empty() {
        return Err(VmError::Runtime(
            "provider_io_disposition expects no arguments".to_string(),
        ));
    }
    let disposition = mock::provider_io_disposition();
    let mut result = std::collections::BTreeMap::new();
    result.put_bool("provider_io", disposition.provider_io());
    result.put_str("source", disposition.source());
    Ok(VmValue::dict(result))
}

/// Register a legacy v0 inline LLM mock response for tests.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "llm_mock(config: dict) -> nil", category = "llm.mock"
)]
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
///
/// Reached from source as `HarnessLlm.mock_load_jsonl`: a script that already
/// holds the llm capability and read the fixture through its own fs grant has
/// the authority this needs, and routing it through the handle is what lets a
/// pipeline install a versioned fixture without a host round-trip.
#[harn_builtin(
    exposure = "harness.llm.mock_load_jsonl",
    effects = [],
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
    result.put_int("schema_version", i64::from(receipt.schema_version));
    result.put_bool("strict_scopes", receipt.strict_scopes);
    let mut provider_io = std::collections::BTreeMap::new();
    provider_io.put_bool("cli_replay", receipt.provider_io.cli_replay);
    provider_io.put_bool("builtin_replay", receipt.provider_io.builtin_replay);
    result.put("provider_io", VmValue::dict(provider_io));
    if let Some(live_after_calls) = receipt.live_after_calls {
        result.put_int(
            "live_after_calls",
            i64::try_from(live_after_calls).unwrap_or(i64::MAX),
        );
    }
    result.put_int("count", receipt.count as i64);
    result.put("scopes", VmValue::strings(receipt.scopes));
    result.put("warnings", VmValue::strings(receipt.warnings));
    Ok(VmValue::dict(result))
}

/// Return the remaining queue as a pure Harn-owned snapshot. Turn-end Harn
/// code uses this to checkpoint queue state without reaching into Rust's
/// mutable fixture storage.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "llm_mock_snapshot() -> dict", category = "llm.mock"
)]
fn llm_mock_snapshot_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(json_to_vm_value(&mock::builtin_llm_mock_snapshot()))
}

/// Return Harn's advisory purpose vocabulary. Fixture scopes remain open
/// strings; this list lets Harn callers discover the purposes it assigns.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "llm_mock_known_scopes() -> list", category = "llm.mock"
)]
fn llm_mock_known_scopes_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::strings(mock::KNOWN_MOCK_SCOPES.iter().copied()))
}

/// Return recorded LLM mock calls.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "llm_mock_calls() -> list", category = "llm.mock"
)]
fn llm_mock_calls_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let calls = mock::get_llm_mock_calls();
    let result: Vec<VmValue> = calls
        .iter()
        .map(|c| {
            let mut dict = std::collections::BTreeMap::new();
            dict.put_str("mock_scope", c.mock_scope.as_str());
            dict.put_str("api_mode", c.api_mode.as_str());
            let messages: Vec<VmValue> = c.messages.iter().map(json_to_vm_value).collect();
            dict.put("messages", VmValue::list(messages));
            dict.insert(
                "system".to_string(),
                match &c.system {
                    Some(s) => VmValue::string(s.as_str()),
                    None => VmValue::Nil,
                },
            );
            dict.insert(
                "tools".to_string(),
                match &c.tools {
                    Some(t) => {
                        let tools: Vec<VmValue> = t.iter().map(json_to_vm_value).collect();
                        VmValue::list(tools)
                    }
                    None => VmValue::Nil,
                },
            );
            dict.insert(
                "provider_tools".to_string(),
                match &c.provider_tools {
                    Some(t) => {
                        let tools: Vec<VmValue> = t.iter().map(json_to_vm_value).collect();
                        VmValue::list(tools)
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
            dict.put_int("max_tokens", c.max_tokens);
            dict.put_nullable("temperature", c.temperature.map(VmValue::Float));
            dict.put_nullable("top_p", c.top_p.map(VmValue::Float));
            dict.put_nullable("top_k", c.top_k.map(VmValue::Int));
            dict.put_nullable(
                "stop",
                c.stop
                    .as_ref()
                    .map(|items| VmValue::strings(items.iter().map(|item| item.as_str()))),
            );
            dict.put_nullable("seed", c.seed.map(VmValue::Int));
            dict.put_nullable("frequency_penalty", c.frequency_penalty.map(VmValue::Float));
            dict.put_nullable("presence_penalty", c.presence_penalty.map(VmValue::Float));
            dict.put_nullable_str("previous_response_id", c.previous_response_id.as_deref());
            dict.put_nullable("store", c.store.map(VmValue::Bool));
            dict.put_nullable("background", c.background.map(VmValue::Bool));
            dict.put_nullable_str("truncation", c.truncation.as_deref());
            dict.put_nullable("compact", c.compact.map(VmValue::Bool));
            dict.put_nullable(
                "include",
                c.include
                    .as_ref()
                    .map(|items| VmValue::strings(items.iter().map(|item| item.as_str()))),
            );
            dict.put_nullable("max_tool_calls", c.max_tool_calls.map(VmValue::Int));
            dict.put_nullable_str("prefill", c.prefill.as_deref());
            VmValue::dict(dict)
        })
        .collect();
    Ok(VmValue::list(result))
}

/// Return the scope-consumption receipts emitted since the last clear.
/// Each receipt records requested/resolved scope, fallback, consumption, and
/// the post-match remaining count without exposing queue internals.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "llm_mock_receipts() -> list", category = "llm.mock"
)]
fn llm_mock_receipts_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let receipts = mock::get_llm_mock_receipts();
    let result: Vec<VmValue> = receipts
        .iter()
        .map(|receipt| {
            let mut dict = std::collections::BTreeMap::new();
            dict.put_str("requested_scope", receipt.requested_scope.as_str());
            dict.put_str("resolved_scope", receipt.resolved_scope.as_str());
            dict.put_bool("matched", receipt.matched);
            dict.put_str("id", receipt.id.as_str());
            dict.put_str("consume", receipt.consume.as_str());
            dict.put_bool("fell_through", receipt.fell_through);
            dict.put_int("remaining", receipt.remaining as i64);
            VmValue::dict(dict)
        })
        .collect();
    Ok(VmValue::list(result))
}

/// Clear deterministic LLM mocks and recorded calls.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "llm_mock_clear() -> nil", category = "llm.mock"
)]
fn llm_mock_clear_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    mock::reset_llm_mock_state();
    Ok(VmValue::Nil)
}

/// Push an isolated LLM mock scope.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "llm_mock_push_scope() -> nil", category = "llm.mock"
)]
fn llm_mock_push_scope_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    mock::push_llm_mock_scope();
    Ok(VmValue::Nil)
}

/// Pop the current isolated LLM mock scope.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "llm_mock_pop_scope() -> nil", category = "llm.mock"
)]
fn llm_mock_pop_scope_builtin(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if !mock::pop_llm_mock_scope() {
        return Err(VmError::Thrown(VmValue::string(
            "llm_mock_pop_scope: no scope to pop",
        )));
    }
    Ok(VmValue::Nil)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_disposition_receipt_is_independent_of_ambient_cli_policy() {
        mock::reset_llm_mock_state();
        mock::install_cli_llm_mocks(Vec::new());
        for (text, cli_replay, builtin_replay) in [
            ("{\"text\":\"legacy\"}\n", false, true),
            (
                "{\"schemaVersion\":1,\"strictScopes\":false}\n",
                false,
                false,
            ),
            (
                include_str!("testdata/live_prefix_fixture.jsonl"),
                true,
                true,
            ),
        ] {
            mock::push_llm_mock_scope();
            let receipt = llm_mock_load_jsonl_builtin(&[VmValue::string(text)], &mut String::new())
                .expect("canonical fixture parses");
            let VmValue::Dict(receipt) = receipt else {
                panic!("fixture receipt")
            };
            let Some(VmValue::Dict(io)) = receipt.get("provider_io") else {
                panic!("typed I/O receipt")
            };
            assert!(
                matches!(io.get("cli_replay"), Some(VmValue::Bool(value)) if *value == cli_replay)
            );
            assert!(
                matches!(io.get("builtin_replay"), Some(VmValue::Bool(value)) if *value == builtin_replay)
            );
            assert_eq!(
                mock::provider_io_disposition(),
                mock::ProviderIoDisposition::CliReplay,
                "ambient protection must not change the parsed child policy"
            );
            assert!(mock::pop_llm_mock_scope());
        }
        mock::clear_cli_llm_mock_mode();
        assert_eq!(
            mock::provider_io_disposition(),
            mock::ProviderIoDisposition::Possible
        );
    }

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
            &[VmValue::string(
                "{\"schemaVersion\":1,\"strictScopes\":false}\n{not json}\n",
            )],
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
            &[VmValue::string("{\"schemaVersion\":1,\"strictScopes\":true}\n\
                 {\"id\":\"judge-1\",\"scope\":\"completion.judge\",\"consume\":\"sticky\",\"match\":\"*\",\"text\":\"JUDGE\"}\n")],
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
            mock::mock_llm_response(&request(Some("completion.judge")))
                .expect("scoped fixture")
                .text,
            "JUDGE"
        );
        mock::push_llm_mock_scope();
        assert!(mock::pop_llm_mock_scope(), "fixture scope restores");
        assert_eq!(
            mock::mock_llm_response(&request(Some("completion.judge")))
                .expect("restored scoped fixture")
                .text,
            "JUDGE"
        );
        assert!(
            mock::mock_llm_response(&request(None)).is_err(),
            "strict builtin fixtures must hard-miss instead of using the generated fallback"
        );

        let inline = VmValue::dict([("text", VmValue::string("must not mix"))]);
        let error = llm_mock_builtin(&[inline], &mut out)
            .expect_err("inline v0 entry must not mutate a v1 fixture");
        assert!(error.to_string().contains("active versioned fixture"));
        mock::reset_llm_mock_state();
    }

    #[test]
    fn inline_mock_uses_the_canonical_v0_decoder() {
        mock::reset_llm_mock_state();
        let mut out = String::new();
        let inline = VmValue::dict([
            ("match", VmValue::string("*")),
            ("scope", VmValue::string("completion.judge")),
            ("text", VmValue::string("V0")),
        ]);
        // harn#7864: v0 used to accept `scope` and drop it, which left the
        // author believing an entry was bucketed when it was not. The inline
        // builtin shares the canonical v0 decoder, so it now refuses the key
        // and names the header that would have given it meaning.
        let error = llm_mock_builtin(&[inline], &mut out)
            .expect_err("a v1 field in a headerless entry must not be dropped");
        let error = error.to_string();
        assert!(error.contains("scope"), "{error}");
        assert!(error.contains("schemaVersion"), "{error}");

        // Without it, the historical default-queue behavior is unchanged: a
        // v0 entry serves every scope from the one legacy bucket.
        let inline = VmValue::dict([
            ("match", VmValue::string("*")),
            ("text", VmValue::string("V0")),
        ]);
        llm_mock_builtin(&[inline], &mut out).expect("inline v0 mock");
        assert_eq!(
            mock::mock_llm_response(&request(Some("completion.judge")))
                .expect("v0 fallback")
                .text,
            "V0"
        );
        mock::reset_llm_mock_state();
    }

    #[test]
    fn load_reports_warnings_and_snapshot_preserves_scope_queues() {
        mock::reset_llm_mock_state();
        let mut out = String::new();
        let receipt = llm_mock_load_jsonl_builtin(
            &[VmValue::string("{\"schemaVersion\":1,\"strictScopes\":false}\n\
                 {\"id\":\"custom-1\",\"scope\":\"custom_review\",\"consume\":\"once\",\"text\":\"CUSTOM\"}\n\
                 {\"id\":\"main-1\",\"scope\":\"agent.main\",\"consume\":\"once\",\"text\":\"MAIN\"}\n")],
            &mut out,
        )
        .expect("valid fixture");
        let VmValue::Dict(receipt) = receipt else {
            panic!("load receipt must be a dict");
        };
        let Some(VmValue::List(warnings)) = receipt.get("warnings") else {
            panic!("warnings must be returned");
        };
        assert_eq!(warnings.len(), 1);

        let VmValue::Dict(snapshot) = llm_mock_snapshot_builtin(&[], &mut out).expect("snapshot")
        else {
            panic!("snapshot must be a dict");
        };
        assert!(matches!(
            snapshot.get("schema"),
            Some(VmValue::String(schema)) if schema.as_str() == "harn.llm_mock_fixture_queue.v1"
        ));
        let VmValue::Dict(remaining) = snapshot.get("queue_remaining").expect("queue_remaining")
        else {
            panic!("queue_remaining must be a dict");
        };
        assert!(matches!(remaining.get("agent.main"), Some(VmValue::Int(1))));
        let VmValue::List(known_scopes) =
            llm_mock_known_scopes_builtin(&[], &mut out).expect("known scopes")
        else {
            panic!("known scopes must be a list");
        };
        assert!(known_scopes.iter().any(
            |scope| matches!(scope, VmValue::String(scope) if scope.as_str() == "agent.main")
        ));
        mock::reset_llm_mock_state();
    }
}
