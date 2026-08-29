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

#[test]
fn exact_promotion_boundaries_surface_in_generated_contracts() {
    let schema = schema_value();
    assert_eq!(
        schema["$defs"]["promotional_pricing"]["properties"]["starts_at"]["format"],
        "date-time"
    );
    assert_eq!(
        schema["$defs"]["promotional_pricing"]["properties"]["ends_at"]["format"],
        "date-time"
    );

    let typescript = typescript_declarations();
    assert!(typescript.contains("starts_at?: string"));
    assert!(typescript.contains("ends_at?: string"));

    let swift = swift_binding().expect("Swift binding renders");
    assert!(swift.contains("public let startsAt: String?"));
    assert!(swift.contains("public let endsAt: String?"));
    assert!(swift.contains("case endsAt = \"ends_at\""));
}
