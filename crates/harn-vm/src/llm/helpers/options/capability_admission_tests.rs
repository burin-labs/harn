use super::routing_test_support::{extract_with_options, ScopedEnvVar};
use super::*;

fn explicit_option_error(option: &str, value: VmValue) -> VmError {
    let options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("local")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("unsupported-generation")),
        ),
        (crate::value::intern_key(option), value),
    ]);
    extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("hello")),
        VmValue::Nil,
        VmValue::dict(options),
    ])
    .expect_err("authored generation denial must reject")
}

fn gemini_interactions_options() -> crate::value::DictMap {
    crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("gemini")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("gemini-3.5-flash-lite")),
        ),
    ])
}

#[test]
fn portable_generation_denials_share_one_typed_admission_gate() {
    crate::llm::capabilities::set_user_overrides_toml(concat!(
        "[[provider.local]]\n",
        "model_match = \"unsupported-generation\"\n",
        "temperature_supported = false\n",
        "top_p_supported = false\n",
        "top_k_supported = false\n",
        "seed_supported = false\n",
        "frequency_penalty_supported = false\n",
        "presence_penalty_supported = false\n",
        "stop_supported = false\n",
    ))
    .expect("explicit generation capability fixture");

    let cases = [
        ("temperature", VmValue::Float(0.2)),
        ("top_p", VmValue::Float(0.9)),
        ("top_k", VmValue::Int(20)),
        ("seed", VmValue::Int(42)),
        ("frequency_penalty", VmValue::Float(0.2)),
        ("presence_penalty", VmValue::Float(0.2)),
        (
            "stop",
            VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                arcstr::ArcStr::from("END"),
            )])),
        ),
    ];
    for (option, value) in cases {
        let error = explicit_option_error(option, value);
        assert!(
            error.to_string().contains(&format!(
                "option `{option}` is not supported by `unsupported-generation` (provider `local`)."
            )),
            "unexpected error for {option}: {error}"
        );
        assert!(error.to_string().contains("harn provider catalog matrix"));
    }

    crate::llm::capabilities::clear_user_overrides();
}

#[test]
fn gemini_interactions_penalties_fail_before_request_construction() {
    let _guard = crate::llm::env_guard();
    // The shared guard restores this inert test credential before another test
    // can observe it. Extraction reaches admission but never dispatches.
    let _key = ScopedEnvVar::set("GEMINI_API_KEY", "test-key");

    for (option, value) in [
        ("frequency_penalty", VmValue::Float(0.2)),
        ("presence_penalty", VmValue::Float(0.2)),
    ] {
        let mut options = gemini_interactions_options();
        options.insert(crate::value::intern_key(option), value);
        let error = extract_with_options(options)
            .expect_err("Interactions must reject a portable penalty before transport");
        assert!(
            error.to_string().contains(&format!(
                "option `{option}` is not supported by `gemini-3.5-flash-lite` (provider `gemini`)."
            )),
            "unexpected error for {option}: {error}"
        );
    }

    extract_with_options(gemini_interactions_options())
        .expect("an Interactions call without penalties remains admissible");
}
