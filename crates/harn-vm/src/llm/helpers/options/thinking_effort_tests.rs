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

#[test]
fn unsupported_effort_returns_a_typed_local_safe_error() {
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.local]]
model_match = "effort-level-gated"
thinking_modes = ["effort"]
reasoning_effort_supported = true
reasoning_effort_levels = ["low", "medium", "high"]
"#,
    )
    .expect("capability override");

    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("local")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("effort-level-gated")),
        ),
        (
            crate::value::intern_key("effort"),
            VmValue::String(arcstr::ArcStr::from("none")),
        ),
    ]);
    let err = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello")),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect_err("unsupported effort must fail before dispatch");
    crate::llm::capabilities::clear_user_overrides();

    let safe = crate::llm::call::llm_safe_envelope_err(&err);
    let error = safe
        .as_dict()
        .and_then(|safe| safe.get("error"))
        .and_then(VmValue::as_dict)
        .expect("safe error envelope");

    assert_eq!(
        error.get("category").map(VmValue::display).as_deref(),
        Some("invalid_request")
    );
    assert_eq!(
        error.get("kind").map(VmValue::display).as_deref(),
        Some("terminal")
    );
    assert_eq!(
        error.get("reason").map(VmValue::display).as_deref(),
        Some("invalid_request")
    );
    assert_eq!(
        error.get("origin").map(VmValue::display).as_deref(),
        Some("local")
    );
    assert_eq!(
        error.get("provider").map(VmValue::display).as_deref(),
        Some("local")
    );
    assert_eq!(
        error.get("model").map(VmValue::display).as_deref(),
        Some("effort-level-gated")
    );
}
