use super::*;

#[test]
fn blocked_response_contains_every_process_result_field() {
    let response = blocked_command_response(
        &crate::value::DictMap::new(),
        "blocked",
        "denied by test",
        serde_json::json!({}),
        Vec::new(),
    );
    let VmValue::Dict(response) = response else {
        panic!("blocked process response must be a record");
    };
    let harn_builtin_meta::Ty::Shape(fields) = harn_builtin_meta::shapes::PROCESS_RESULT else {
        panic!("PROCESS_RESULT must remain a closed record");
    };

    for field in fields {
        assert!(
            response.contains_key(field.name),
            "blocked process response is missing `{}`",
            field.name,
        );
    }
    assert!(!response.contains_key("exit_status"));
    assert!(!response.contains_key("legacy_status"));
}
