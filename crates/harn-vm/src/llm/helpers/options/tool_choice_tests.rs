use super::extract::*;
use super::*;

/// Opus 5.5 and Fable 5.1 answer a forced tool choice with a 400. The option
/// boundary lowers it to `auto` on any route whose catalog row forbids forcing,
/// and leaves it alone everywhere else (the control route below).
#[test]
fn forced_tool_choice_is_relaxed_only_where_the_catalog_forbids_it() {
    crate::llm::capabilities::clear_user_overrides();
    crate::llm_config::clear_user_overrides();
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.mock]]
model_match = "forcing-rejected"
native_tools = true
allowed_tool_choice_modes = ["auto", "none"]

[[provider.mock]]
model_match = "forcing-allowed"
native_tools = true
"#,
    )
    .expect("mock tool-choice capability override");

    let tool_choice_for = |model: &str, choice: serde_json::Value| {
        let options = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("mock")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from(model.to_string())),
            ),
            (
                crate::value::intern_key("tool_choice"),
                crate::stdlib::json_to_vm_value(&choice),
            ),
        ]);
        extract_llm_options(&[
            VmValue::String(arcstr::ArcStr::from("hello")),
            VmValue::Nil,
            VmValue::dict(options),
        ])
        .expect("options")
        .tool_choice
    };

    assert_eq!(
        tool_choice_for("forcing-rejected", serde_json::json!("required")),
        Some(serde_json::json!("auto"))
    );
    assert_eq!(
        tool_choice_for(
            "forcing-rejected",
            serde_json::json!({"type": "tool", "name": "edit", "disable_parallel_tool_use": true})
        ),
        Some(serde_json::json!({"type": "auto", "disable_parallel_tool_use": true}))
    );
    assert_eq!(
        tool_choice_for("forcing-rejected", serde_json::json!("none")),
        Some(serde_json::json!("none"))
    );
    assert_eq!(
        tool_choice_for("forcing-allowed", serde_json::json!("required")),
        Some(serde_json::json!("required"))
    );
    crate::llm::capabilities::clear_user_overrides();
}
