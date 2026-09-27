use super::extract::extract_llm_options;
use crate::value::VmValue;

fn one_tool_list() -> VmValue {
    VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
        std::sync::Arc::new(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("name"),
                VmValue::String(arcstr::ArcStr::from("lookup")),
            ),
            (
                crate::value::intern_key("description"),
                VmValue::String(arcstr::ArcStr::from("Look something up")),
            ),
            (
                crate::value::intern_key("parameters"),
                VmValue::dict(crate::value::DictMap::new()),
            ),
        ])),
    )]))
}

fn forced_native_options(
    native_tools: bool,
    override_reason: &str,
) -> crate::llm::api::LlmCallOptions {
    crate::llm::capabilities::clear_user_overrides();
    crate::llm_config::clear_user_overrides();
    crate::llm::capabilities::set_user_overrides_toml(&format!(
        r#"
[[provider.local]]
model_match = "forced-native-model"
native_tools = {native_tools}
preferred_tool_format = "text"
text_tool_wire_format_supported = true
tool_mode_parity = "text_only"
"#,
    ))
    .expect("forced-native capability override");

    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("local")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("forced-native-model")),
        ),
        (
            crate::value::intern_key("tool_format"),
            VmValue::String(arcstr::ArcStr::from("native")),
        ),
        (
            crate::value::intern_key("tool_format_override_reason"),
            VmValue::String(arcstr::ArcStr::from(override_reason)),
        ),
        (crate::value::intern_key("tools"), one_tool_list()),
        // A blank reason steers to the text channel, which only the agent
        // loop may drive; model that caller so the steering itself is tested.
        (
            crate::value::intern_key("_tool_contract_rendered"),
            VmValue::Bool(true),
        ),
    ]);
    let result = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello")),
        VmValue::Nil,
        VmValue::dict(options),
    ]);
    crate::llm::capabilities::clear_user_overrides();
    result.expect("deliberately forced native format should be accepted")
}

#[test]
fn tool_format_override_reason_bypasses_native_capability_and_parity_gates() {
    for native_tools in [false, true] {
        let opts = forced_native_options(native_tools, "measure the native channel deliberately");
        assert_eq!(
            opts.native_tools.as_ref().map(Vec::len),
            Some(1),
            "the forced native arm must put its tool schema on the provider wire when native_tools={native_tools}"
        );
    }
}

#[test]
fn blank_tool_format_override_reason_does_not_bypass_channel_gates() {
    let opts = forced_native_options(false, "  ");
    assert!(opts.native_tools.is_none());
}

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
