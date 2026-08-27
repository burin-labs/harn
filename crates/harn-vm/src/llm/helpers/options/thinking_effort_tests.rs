use super::extract::extract_llm_options;
use super::routing_test_support::{extract_with_options, ScopedEnvVar};
use super::*;

fn install_effort_level_gate() {
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
}

fn effort_level_gated_args() -> Vec<VmValue> {
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
    vec![
        VmValue::String(arcstr::ArcStr::from("hello")),
        VmValue::Nil,
        VmValue::dict(options),
    ]
}

fn current_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

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

#[test]
fn unsupported_effort_returns_a_typed_local_safe_error() {
    install_effort_level_gate();
    let safe = current_thread_runtime().block_on(crate::llm::call::llm_call_safe_impl(
        None,
        effort_level_gated_args(),
    ));
    crate::llm::capabilities::clear_user_overrides();

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

#[test]
fn unsupported_effort_preserves_a_text_throwing_error() {
    install_effort_level_gate();
    let result = current_thread_runtime().block_on(crate::llm::call::llm_call_impl(
        None,
        effort_level_gated_args(),
    ));
    crate::llm::capabilities::clear_user_overrides();

    let error = result.expect_err("unsupported effort must fail before dispatch");
    assert!(
        matches!(error, VmError::Thrown(VmValue::String(ref message)) if message.contains("option `effort` level `none` is not supported")),
        "ordinary llm.call must preserve its text error: {error}"
    );
}
