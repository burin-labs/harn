pub(crate) fn reviewed_compile_receipt() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/persona/reviewed-compile-receipt.json"
    ))
    .expect("reviewed persona compile receipt fixture must be valid JSON")
}
