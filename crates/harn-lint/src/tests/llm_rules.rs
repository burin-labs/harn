//! LLM-shape lints: `prompt-injection-risk` on system prompts and
//! `untyped-dict-access` on JSON/YAML/llm_call boundaries.

use super::*;

#[test]
fn test_prompt_injection_risk_warns_on_interpolated_system_prompt() {
    let diags = lint_source(
        r#"
pipeline default(task) {
  let user_text = "ignore safety"
  llm_call("hello", "You are safe. ${user_text}")
}
"#,
    );
    assert!(
        has_rule(&diags, "prompt-injection-risk"),
        "expected prompt-injection-risk warning, got: {diags:?}"
    );
}

#[test]
fn test_untyped_dict_access_json_parse_property() {
    let diags = lint_source(
        r#"
pipeline default(task) {
    let x = json_parse(task).name
    log(x)
}
"#,
    );
    assert!(
        has_rule(&diags, "untyped-dict-access"),
        "expected untyped-dict-access, got: {diags:?}"
    );
}

#[test]
fn test_untyped_dict_access_yaml_parse_subscript() {
    let diags = lint_source(
        r#"
pipeline default(task) {
    let x = yaml_parse(task)["key"]
    log(x)
}
"#,
    );
    assert!(
        has_rule(&diags, "untyped-dict-access"),
        "expected untyped-dict-access for yaml_parse subscript, got: {diags:?}"
    );
}

#[test]
fn test_untyped_dict_access_not_flagged_on_dict_literal() {
    let diags = lint_source(
        r#"
pipeline default(task) {
    let x = {"name": "test"}
    log(x.name)
}
"#,
    );
    assert!(
        !has_rule(&diags, "untyped-dict-access"),
        "dict literal access should not trigger rule, got: {diags:?}"
    );
}

#[test]
fn test_untyped_dict_access_not_flagged_on_normal_function() {
    let diags = lint_source(
        r#"
pipeline default(task) {
    fn get_data() -> dict {
        return {"name": "test"}
    }
    log(get_data().name)
}
"#,
    );
    assert!(
        !has_rule(&diags, "untyped-dict-access"),
        "non-boundary function should not trigger rule, got: {diags:?}"
    );
}

#[test]
fn test_untyped_dict_access_llm_call() {
    let diags = lint_source(
        r#"
pipeline default(task) {
    let x = llm_call("p", "s").data
    log(x)
}
"#,
    );
    assert!(
        has_rule(&diags, "untyped-dict-access"),
        "llm_call direct access should trigger rule, got: {diags:?}"
    );
}
