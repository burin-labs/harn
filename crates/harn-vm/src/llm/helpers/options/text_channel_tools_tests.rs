use super::extract::extract_llm_options;
use super::routing_test_support::one_tool_list;
use crate::value::VmValue;

fn text_channel_options(tool_format: &str, contract_rendered: bool) -> crate::value::DictMap {
    let mut options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("local")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("text-channel-model")),
        ),
        (
            crate::value::intern_key("tool_format"),
            VmValue::String(arcstr::ArcStr::from(tool_format)),
        ),
        (crate::value::intern_key("tools"), one_tool_list()),
    ]);
    if contract_rendered {
        options.insert(
            crate::value::intern_key("_tool_contract_rendered"),
            VmValue::Bool(true),
        );
    }
    options
}

/// A direct call with text-channel tools sends no schemas and no contract, so
/// the model is never told a tool exists. It must refuse, not dispatch.
#[test]
fn a_direct_call_with_text_channel_tools_is_refused() {
    crate::llm::capabilities::clear_user_overrides();
    crate::llm_config::clear_user_overrides();
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.local]]
model_match = "text-channel-model"
native_tools = true
preferred_tool_format = "text"
text_tool_wire_format_supported = true
"#,
    )
    .expect("text-channel capability override");
    for tool_format in ["json", "text"] {
        let refused = extract_llm_options(&[
            VmValue::String(arcstr::ArcStr::from("hello")),
            VmValue::Nil,
            VmValue::dict(text_channel_options(tool_format, false)),
        ]);
        let message = match refused {
            Err(crate::value::VmError::Thrown(VmValue::Dict(fields))) => fields
                .get("message")
                .map(VmValue::display)
                .unwrap_or_default(),
            Err(other) => format!("{other:?}"),
            Ok(_) => panic!("tool_format {tool_format}: a direct call with tools must be refused"),
        };
        assert!(
            message.contains("agent_loop") && message.contains("tool_format: \"native\""),
            "tool_format {tool_format}: {message}"
        );
        // Negative control: the agent loop's own call renders the contract.
        let accepted = extract_llm_options(&[
            VmValue::String(arcstr::ArcStr::from("hello")),
            VmValue::Nil,
            VmValue::dict(text_channel_options(tool_format, true)),
        ]);
        assert!(
            accepted.is_ok(),
            "tool_format {tool_format}: the loop-marked call must be accepted: {:?}",
            accepted.err()
        );
    }
    crate::llm::capabilities::clear_user_overrides();
}

#[test]
fn text_tool_format_does_not_emit_native_provider_tools() {
    crate::llm::capabilities::clear_user_overrides();
    crate::llm_config::clear_user_overrides();

    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("ollama".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("devstral-small-2:24b".to_string())),
        ),
        (
            crate::value::intern_key("tool_format"),
            VmValue::String(arcstr::ArcStr::from("text".to_string())),
        ),
        (crate::value::intern_key("tools"), one_tool_list()),
        // The agent loop's call, which renders the text contract.
        (
            crate::value::intern_key("_tool_contract_rendered"),
            VmValue::Bool(true),
        ),
    ]);
    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("text-format tools accepted");

    assert!(opts.tools.is_some());
    assert!(opts.native_tools.is_none());
}
