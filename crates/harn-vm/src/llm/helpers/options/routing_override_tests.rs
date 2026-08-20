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
