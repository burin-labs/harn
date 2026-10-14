use super::{extract_json_from_text, json_extract_impl};
use crate::value::VmValue;

#[test]
fn extract_return_value_after_code_in_the_explanation() {
    let answer = r#"{"verdict":"confirmed","change_caused":true,"breaks":true}"#;
    let text = format!("The release saves `memory + {{phase: phase}}`.\n\n{answer}");
    let result = json_extract_impl(&[VmValue::string(text)], &mut String::new())
        .expect("a code expression must not hide the child's typed return");
    assert_eq!(
        result.as_dict().unwrap().get("verdict").unwrap().display(),
        "confirmed"
    );
    assert!(
        json_extract_impl(&[VmValue::string("{phase: phase}")], &mut String::new()).is_err(),
        "code alone must not become a measured return"
    );
}

#[test]
fn rejected_code_spans_cannot_supply_nested_json() {
    let answer = r#"{"verdict":"confirmed"}"#;
    for (text, expected) in [
        (
            format!("[phase: {{\"verdict\":\"draft\"}}] {answer}"),
            answer,
        ),
        (
            format!("```python\npayload = {{\"phase\":\"draft\"}}\n```\n{answer}"),
            answer,
        ),
        (
            "The example is {phase: [1, 2]} and the return is [3, 4]".to_string(),
            "[3, 4]",
        ),
        (
            format!("The example is {{phase: phase}} and the return is {answer}"),
            answer,
        ),
        (
            "```python\nphase = phase\n```\n[3, 4]".to_string(),
            "[3, 4]",
        ),
        (
            format!("```harn\n{{phase: phase}}\n```\n```json\n{answer}\n```"),
            answer,
        ),
        (format!("```json\n{answer}\n```"), answer),
    ] {
        assert_eq!(extract_json_from_text(&text), expected, "{text}");
    }
}
