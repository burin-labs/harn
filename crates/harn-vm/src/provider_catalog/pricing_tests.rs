use super::*;

#[test]
fn serving_tier_response_values_surface_in_generated_contracts() {
    let schema = schema_value();
    assert_eq!(
        schema["$defs"]["serving_tier_request"]["properties"]["response_values"]["uniqueItems"],
        true
    );

    let typescript = typescript_declarations();
    assert!(typescript.contains("response_values?: string[]"));

    let swift = swift_binding().expect("Swift binding renders");
    assert!(swift.contains("public let responseValues: [String]?"));
}
