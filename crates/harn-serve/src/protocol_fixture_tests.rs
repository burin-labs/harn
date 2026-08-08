use serde_json::Value;
use std::{fs, path::Path};

pub(crate) fn assert_fixture_documents_match(fixture_name: &str, actual: Vec<Value>) {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(fixture_name);
    let fixture_json = fs::read_to_string(&fixture_path).unwrap_or_else(|error| {
        panic!(
            "failed to read protocol fixture {}: {error}",
            fixture_path.display()
        )
    });
    let fixture: Value = serde_json::from_str(&fixture_json).expect("protocol fixture json");
    let expected = fixture
        .get("documents")
        .and_then(Value::as_array)
        .expect("protocol fixture documents")
        .clone();
    let expected_value = Value::Array(expected);
    let actual_value = Value::Array(actual);
    assert!(
        actual_value == expected_value,
        "protocol fixture drifted: {fixture_name}\nexpected:\n{}\nactual:\n{}",
        serde_json::to_string_pretty(&expected_value).expect("expected json"),
        serde_json::to_string_pretty(&actual_value).expect("actual json")
    );
}
