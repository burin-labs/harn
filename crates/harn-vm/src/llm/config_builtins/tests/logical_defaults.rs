//! Logical-model defaults resolved through an alias.

use super::super::selection_builtins::llm_model_defaults_builtin;
use crate::llm_config;
use crate::value::VmValue;

#[test]
fn model_defaults_builtin_resolves_logical_defaults_from_alias() {
    llm_config::clear_user_overrides();
    let mut out = String::new();
    let args = vec![VmValue::String(arcstr::ArcStr::from(
        "baseten-gpt-oss-120b",
    ))];
    let result = llm_model_defaults_builtin(&args, &mut out).expect("logical defaults");
    let dict = result.as_dict().expect("expected dict");
    match dict.get("temperature") {
        Some(VmValue::Float(value)) => assert_eq!(*value, 1.0),
        other => panic!("expected temperature=1.0, got {other:?}"),
    }
    assert!(!dict.contains_key("top_p"));
    assert_eq!(
        dict.get("reasoning_effort")
            .map(VmValue::display)
            .as_deref(),
        Some("high")
    );
}
