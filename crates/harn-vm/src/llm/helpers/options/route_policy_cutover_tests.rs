use super::extract::extract_llm_options;
use super::*;
use crate::value::{intern_key, DictMap, VmDictExt};

fn extract(policy: DictMap) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    let options = DictMap::from_iter([
        (intern_key("provider"), VmValue::String("mock".into())),
        (intern_key("model"), VmValue::String("gpt-5.4".into())),
        (intern_key("route_policy"), VmValue::dict(policy)),
    ]);
    extract_llm_options(&[
        VmValue::String("hello".into()),
        VmValue::Nil,
        VmValue::dict(options),
    ])
}

fn message(error: VmError) -> String {
    match error {
        VmError::Thrown(VmValue::String(message)) => message.to_string(),
        other => panic!("expected thrown route-policy error, got {other:?}"),
    }
}

#[test]
fn route_policy_dict_rejects_unknown_key() {
    let mut policy = DictMap::new();
    policy.put_str("mode", "cheapest_over_quality");
    policy.put_str("quality", "mid");
    let error = message(extract(policy).expect_err("unknown key must fail"));
    assert!(error.contains("unknown key `quality`"), "{error}");
    assert!(
        error.contains("mode, target?, targets?, strategy?"),
        "{error}"
    );
}

#[test]
fn route_policy_dict_rejects_removed_prefer_mode() {
    let mut policy = DictMap::new();
    policy.put_str("mode", "prefer");
    policy.put(
        "targets",
        VmValue::List(std::sync::Arc::new(vec![VmValue::String(
            "mock:gpt-5.4".into(),
        )])),
    );
    let error = message(extract(policy).expect_err("removed mode must fail"));
    assert!(error.contains("unsupported value `prefer`"), "{error}");
    assert!(error.contains("preference_list"), "{error}");
}

#[test]
fn route_policy_dict_rejects_prefer_targets_alias() {
    let mut policy = DictMap::new();
    policy.put_str("mode", "preference_list");
    policy.put(
        "prefer",
        VmValue::List(std::sync::Arc::new(vec![VmValue::String(
            "mock:gpt-5.4".into(),
        )])),
    );
    let error = message(extract(policy).expect_err("removed targets alias must fail"));
    assert!(error.contains("unknown key `prefer`"), "{error}");
}
