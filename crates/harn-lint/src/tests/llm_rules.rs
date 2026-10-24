//! LLM-shape lints: `prompt-injection-risk` on system prompts and
//! `untyped-dict-access` on JSON/YAML/llm_call boundaries.

use super::*;

#[rstest::rstest]
#[case::test_prompt_injection_risk_warns_on_interpolated_system_prompt(
    r#"
pipeline default(task) {
  const user_text = "ignore safety"
  llm_call("hello", "You are safe. ${user_text}")
}
"#,
    "prompt-injection-risk",
    true
)]
#[case::test_untyped_dict_access_json_parse_property(
    r"
pipeline default(task) {
    const x = json_parse(task).name
    log(x)
}
",
    "untyped-dict-access",
    true
)]
#[case::test_untyped_dict_access_yaml_parse_subscript(
    r#"
pipeline default(task) {
    const x = yaml_parse(task)["key"]
    log(x)
}
"#,
    "untyped-dict-access",
    true
)]
#[case::test_untyped_dict_access_not_flagged_on_dict_literal(
    r#"
pipeline default(task) {
    const x = {"name": "test"}
    log(x.name)
}
"#,
    "untyped-dict-access",
    false
)]
#[case::test_untyped_dict_access_not_flagged_on_normal_function(
    r#"
pipeline default(task) {
    fn get_data() -> dict {
        return {"name": "test"}
    }
    log(get_data().name)
}
"#,
    "untyped-dict-access",
    false
)]
#[case::test_untyped_dict_access_llm_call(
    r#"
pipeline default(task) {
    const x = llm_call("p", "s").data
    log(x)
}
"#,
    "untyped-dict-access",
    true
)]
fn rule_presence(#[case] source: &str, #[case] rule: &str, #[case] present: bool) {
    let diagnostics = lint_source(source);
    assert_eq!(
        has_rule(&diagnostics, rule),
        present,
        "{rule}: {diagnostics:?}"
    );
}
