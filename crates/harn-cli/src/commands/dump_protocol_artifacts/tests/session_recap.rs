use serde_json::json;

use super::*;

fn verification(value: &mut serde_json::Value) -> &mut serde_json::Value {
    &mut value["snapshot"]["turns"][0]["iterations"][0]["tools"][0]["verification"]
}

#[test]
fn generated_bindings_expose_one_session_recap_contract() {
    let bindings = [
        ("Rust", generate_rust()),
        ("Swift", generate_swift()),
        ("TypeScript", generate_typescript()),
        ("Python", generate_python()),
        ("Go", generate_go()),
    ];
    for type_name in [
        "HarnSessionRecapQuery",
        "HarnSessionRecapCoverage",
        "HarnSessionRecapToolExchange",
        "HarnSessionRecapIteration",
        "HarnSessionPromptTurnRecap",
        "HarnSessionRecapSnapshot",
        "HarnSessionRecapAvailability",
    ] {
        for (binding, source) in &bindings {
            assert!(
                source.contains(type_name),
                "{binding} binding omitted {type_name}"
            );
        }
    }
    for (binding, source) in &bindings {
        assert!(
            source.contains(harn_vm::session_recap::SESSION_RECAP_QUERY_METHOD),
            "{binding} binding omitted the recap query method"
        );
    }
}

#[test]
fn schema_and_generated_rust_preserve_the_write_contract() {
    let fixture = super::super::session_recap::session_recap_round_trip_fixture()
        .expect("typed recap fixture");
    let schema = harn_vm::session_recap::session_recap_json_schema();
    jsonschema::draft202012::meta::validate(&schema).expect("recap schema is meta-valid");
    let validator = jsonschema::draft202012::new(&schema).expect("compile recap schema");
    assert!(
        validator.is_valid(&fixture),
        "non-vacuous fixture must validate"
    );

    let decoded: generated_rust_binding::HarnSessionRecapAvailability =
        serde_json::from_value(fixture.clone()).expect("generated Rust binding decodes fixture");
    let encoded = serde_json::to_value(decoded).expect("generated Rust binding re-encodes fixture");
    assert_eq!(
        encoded, fixture,
        "generated Rust binding must preserve every field"
    );
    assert_eq!(
        encoded["snapshot"]["extensions"]["example.harn.dev/recap"]["label"], "fixture",
        "the explicit extension survives the dangerous decode/write direction"
    );

    let mut unknown = fixture;
    unknown["snapshot"]["futureTopLevel"] = json!(true);
    assert!(!validator.is_valid(&unknown));
    assert!(
        serde_json::from_value::<generated_rust_binding::HarnSessionRecapAvailability>(
            unknown.clone()
        )
        .is_err(),
        "generated Rust readers must reject unknown snapshot fields before write-back"
    );

    unknown["snapshot"]
        .as_object_mut()
        .expect("snapshot object")
        .remove("futureTopLevel");
    verification(&mut unknown)["futureNested"] = json!(true);
    assert!(!validator.is_valid(&unknown));
    assert!(
        serde_json::from_value::<generated_rust_binding::HarnSessionRecapAvailability>(
            unknown.clone()
        )
        .is_err(),
        "generated Rust readers must reject unknown nested fields before write-back"
    );
    verification(&mut unknown)
        .as_object_mut()
        .expect("verification object")
        .remove("futureNested");
    verification(&mut unknown)["status"] = json!("future_status");
    assert!(!validator.is_valid(&unknown));
    assert!(
        serde_json::from_value::<generated_rust_binding::HarnSessionRecapAvailability>(unknown)
            .is_err(),
        "generated Rust readers must reject unknown verification statuses"
    );
}
