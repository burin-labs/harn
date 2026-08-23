use super::extract::extract_llm_options;
use super::*;

#[test]
fn thinking_modes_effort_is_the_capability_gate() {
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.local]]
model_match = "thinking-effort-only"
thinking_modes = ["effort"]
"#,
    )
    .expect("capability override");

    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("local".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("thinking-effort-only".to_string())),
        ),
        (
            crate::value::intern_key("effort"),
            VmValue::String(arcstr::ArcStr::from("high".to_string())),
        ),
    ]);
    let opts = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect("thinking_modes containing effort is sufficient");
    assert_eq!(
        opts.thinking,
        crate::llm::api::ThinkingConfig::Effort {
            level: crate::llm::api::ReasoningEffort::High
        }
    );
    crate::llm::capabilities::clear_user_overrides();
}

#[test]
fn effort_option_rejected_when_thinking_modes_omit_effort() {
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.local]]
model_match = "thinking-enabled-only"
thinking_modes = ["enabled"]
"#,
    )
    .expect("capability override");

    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("local".to_string())),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("thinking-enabled-only".to_string())),
        ),
        (
            crate::value::intern_key("effort"),
            VmValue::String(arcstr::ArcStr::from("high".to_string())),
        ),
    ]);
    let err = match extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello".to_string())),
        VmValue::Nil,
        VmValue::dict(options),
    ]) {
        Ok(_) => panic!("unsupported option should fail"),
        Err(err) => err,
    };

    assert!(
        err.to_string().contains("option `effort` is not supported"),
        "unexpected error: {err}"
    );
    crate::llm::capabilities::clear_user_overrides();
}
