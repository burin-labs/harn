use super::extract::extract_llm_options;
use super::routing_test_support::{extract_with_options, ScopedEnvVar};
use super::*;

#[test]
fn moonshot_kimi_k3_accepts_only_its_documented_effort_ladder() {
    let _moonshot_key = ScopedEnvVar::set("MOONSHOT_API_KEY", "test-key");

    for level in ["low", "high", "max"] {
        let options = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("moonshot")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("moonshot/kimi-k3")),
            ),
            (
                crate::value::intern_key("effort"),
                VmValue::String(arcstr::ArcStr::from(level)),
            ),
        ]);
        extract_with_options(options)
            .unwrap_or_else(|err| panic!("documented effort `{level}` was rejected: {err}"));
    }

    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("moonshot")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("moonshot/kimi-k3")),
        ),
        (
            crate::value::intern_key("effort"),
            VmValue::String(arcstr::ArcStr::from("medium")),
        ),
    ]);
    let err = extract_with_options(options).expect_err("undocumented effort must fail locally");
    assert!(
        err.to_string()
            .contains("supported reasoning_effort values: low, high, max"),
        "unexpected error: {err}"
    );
}

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
fn toggle_mode_admits_unbudgeted_thinking_and_rejects_budget() {
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.local]]
model_match = "thinking-toggle-only"
thinking_modes = ["toggle"]
"#,
    )
    .expect("capability override");

    let extract = |thinking| {
        let options = crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("local")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("thinking-toggle-only")),
            ),
            (crate::value::intern_key("thinking"), thinking),
        ]);
        extract_llm_options(&[
            VmValue::String(arcstr::ArcStr::from("hello")),
            VmValue::Nil,
            VmValue::dict(options),
        ])
    };

    let opts = extract(VmValue::Bool(true)).expect("toggle supports provider-managed thinking");
    assert_eq!(
        opts.thinking,
        crate::llm::api::ThinkingConfig::Enabled {
            budget_tokens: None
        }
    );

    let budgeted = VmValue::dict(crate::value::DictMap::from_iter([(
        crate::value::intern_key("budget_tokens"),
        VmValue::Int(128),
    )]));
    let err = extract(budgeted).expect_err("toggle does not accept a numeric thinking budget");
    assert!(
        err.to_string()
            .contains("option `thinking` is not supported"),
        "unexpected error: {err}"
    );
    crate::llm::capabilities::clear_user_overrides();
}
